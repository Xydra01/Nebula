// hydrogen/src/memory/page_allocator.cpp
// Uniform page block container, FP8 E4M3 / Q8_0 dual quantization, and hybrid layer policy

#include "page_block.hpp"
#include <cmath>
#include <cstring>
#include <algorithm>
#include <stdexcept>
#include <numeric>

namespace hydrogen {

namespace {

// Construct the 256-entry IEEE/OCP FP8 E4M3 dequantization lookup table
std::array<float, 256> build_fp8_e4m3_lut() {
    std::array<float, 256> lut{};
    for (int i = 0; i < 256; ++i) {
        uint8_t byte = static_cast<uint8_t>(i);
        bool sign = (byte & 0x80) != 0;
        uint8_t exp = (byte >> 3) & 0x0F;
        uint8_t mant = byte & 0x07;

        float val = 0.0f;
        if (exp == 0) {
            // Subnormal / Zero: (-1)^sign * 2^(-6) * (mant / 8)
            val = std::ldexp(static_cast<float>(mant) / 8.0f, -6);
        } else if (exp == 15 && mant == 7) {
            // Standard E4M3 NaN
            val = std::numeric_limits<float>::quiet_NaN();
        } else {
            // Normal: (-1)^sign * 2^(exp - 7) * (1 + mant / 8)
            float mant_val = 1.0f + (static_cast<float>(mant) / 8.0f);
            val = std::ldexp(mant_val, static_cast<int>(exp) - 7);
        }

        lut[i] = sign ? -val : val;
    }
    return lut;
}

} // anonymous namespace

const std::array<float, 256>& FP8_E4M3::dequant_lut() {
    static const auto lut = build_fp8_e4m3_lut();
    return lut;
}

float FP8_E4M3::to_float(uint8_t byte) {
    return dequant_lut()[byte];
}

uint8_t FP8_E4M3::from_float(float val) {
    if (std::isnan(val)) {
        return 0x7F; // Positive NaN
    }

    bool sign = std::signbit(val);
    float abs_val = std::abs(val);

    if (abs_val <= 0.0f) {
        return sign ? 0x80 : 0x00;
    }

    // Clamp to max normal FP8 E4M3 value (448.0)
    if (abs_val > 448.0f) {
        abs_val = 448.0f;
    }

    // Binary search over the 128 positive values in the LUT
    const auto& lut = dequant_lut();
    uint8_t best_code = 0;
    float best_diff = 1e9f;

    for (uint8_t code = 0; code < 128; ++code) {
        if (std::isnan(lut[code])) continue;
        float diff = std::abs(lut[code] - abs_val);
        if (diff < best_diff) {
            best_diff = diff;
            best_code = code;
        }
    }

    return sign ? static_cast<uint8_t>(best_code | 0x80) : best_code;
}

int8_t Q8_0::from_float(float val, float scale) {
    if (scale <= 0.0f) return 0;
    float scaled = val / scale;
    int q = static_cast<int>(std::round(scaled));
    return static_cast<int8_t>(std::clamp(q, -127, 127));
}

float Q8_0::to_float(int8_t byte, float scale) {
    return static_cast<float>(byte) * scale;
}

PageQuantPolicy::PageQuantPolicy(uint32_t total_layers, uint32_t sensitive_boundary)
    : total_layers_(total_layers), sensitive_boundary_(sensitive_boundary) {}

PageQuantType PageQuantPolicy::get_layer_quant_type(uint32_t layer_idx) const noexcept {
    if (total_layers_ <= sensitive_boundary_ * 2) {
        // Model too small for middle split, protect everything with Q8_0
        return PageQuantType::Q8_0;
    }

    // First sensitive_boundary layers (input projection)
    if (layer_idx < sensitive_boundary_) {
        return PageQuantType::Q8_0;
    }

    // Last sensitive_boundary layers (output projection)
    if (layer_idx >= total_layers_ - sensitive_boundary_) {
        return PageQuantType::Q8_0;
    }

    // Deep intermediate layers use FP8
    return PageQuantType::FP8_E4M3;
}

PageQuantType PageQuantPolicy::evaluate_activation_kurtosis(uint32_t layer_idx, float kurtosis) const noexcept {
    // If standard policy already requires Q8_0, keep Q8_0
    if (get_layer_quant_type(layer_idx) == PageQuantType::Q8_0) {
        return PageQuantType::Q8_0;
    }

    // Heavy-tailed distribution (kurtosis > 5.0) causes FP8 outlier clipping
    if (kurtosis > 5.0f) {
        return PageQuantType::Q8_0;
    }

    return PageQuantType::FP8_E4M3;
}

void PageBlockCodec::encode(
    std::span<const float> src_floats,
    UniformPageBlock& dst_block,
    PageQuantType quant_type
) {
    if (src_floats.size() > PAGE_BLOCK_DATA_BYTES) {
        throw std::invalid_argument("src_floats exceeds PAGE_BLOCK_DATA_BYTES (1280)");
    }

    // Find maximum absolute value for dynamic scaling
    float max_abs = 0.0f;
    for (float f : src_floats) {
        if (!std::isnan(f) && !std::isinf(f)) {
            max_abs = std::max(max_abs, std::abs(f));
        }
    }

    dst_block.quant_type = static_cast<uint8_t>(quant_type);
    dst_block.reserved[0] = 0;
    dst_block.reserved[1] = 0;
    dst_block.reserved[2] = 0;

    if (max_abs <= 1e-12f) {
        dst_block.dynamic_scale = 1.0f;
        std::memset(dst_block.data, 0, PAGE_BLOCK_DATA_BYTES);
        return;
    }

    if (quant_type == PageQuantType::Q8_0) {
        dst_block.dynamic_scale = max_abs / 127.0f;
        for (size_t i = 0; i < src_floats.size(); ++i) {
            dst_block.data[i] = static_cast<uint8_t>(Q8_0::from_float(src_floats[i], dst_block.dynamic_scale));
        }
        // Zero pad remaining bytes
        if (src_floats.size() < PAGE_BLOCK_DATA_BYTES) {
            std::memset(dst_block.data + src_floats.size(), 0, PAGE_BLOCK_DATA_BYTES - src_floats.size());
        }
    } else if (quant_type == PageQuantType::FP8_E4M3) {
        dst_block.dynamic_scale = max_abs / 448.0f;
        float inv_scale = 1.0f / dst_block.dynamic_scale;
        for (size_t i = 0; i < src_floats.size(); ++i) {
            dst_block.data[i] = FP8_E4M3::from_float(src_floats[i] * inv_scale);
        }
        // Zero pad remaining bytes
        if (src_floats.size() < PAGE_BLOCK_DATA_BYTES) {
            std::memset(dst_block.data + src_floats.size(), 0, PAGE_BLOCK_DATA_BYTES - src_floats.size());
        }
    } else {
        throw std::invalid_argument("Unsupported PageQuantType");
    }
}

void PageBlockCodec::decode(
    const UniformPageBlock& src_block,
    std::span<float> dst_floats
) {
    size_t count = std::min(dst_floats.size(), PAGE_BLOCK_DATA_BYTES);

    if (src_block.quant_type == static_cast<uint8_t>(PageQuantType::Q8_0)) {
        float scale = src_block.dynamic_scale;
        for (size_t i = 0; i < count; ++i) {
            int8_t q = static_cast<int8_t>(src_block.data[i]);
            dst_floats[i] = Q8_0::to_float(q, scale);
        }
    } else if (src_block.quant_type == static_cast<uint8_t>(PageQuantType::FP8_E4M3)) {
        float scale = src_block.dynamic_scale;
        const auto& lut = FP8_E4M3::dequant_lut();
        for (size_t i = 0; i < count; ++i) {
            dst_floats[i] = lut[src_block.data[i]] * scale;
        }
    } else {
        throw std::invalid_argument("Corrupt or unknown PageQuantType tag in UniformPageBlock");
    }
}

void PageBlockCodec::measure_fidelity(
    std::span<const float> original,
    std::span<const float> reconstructed,
    double& mse,
    double& max_err,
    double& snr_db
) {
    if (original.size() != reconstructed.size() || original.empty()) {
        throw std::invalid_argument("Sizes must match and be non-empty");
    }

    double sum_sq_err = 0.0;
    double sum_sq_sig = 0.0;
    double max_e = 0.0;

    for (size_t i = 0; i < original.size(); ++i) {
        double orig = static_cast<double>(original[i]);
        double rec = static_cast<double>(reconstructed[i]);
        double err = std::abs(orig - rec);

        sum_sq_err += err * err;
        sum_sq_sig += orig * orig;
        if (err > max_e) {
            max_e = err;
        }
    }

    mse = sum_sq_err / static_cast<double>(original.size());
    max_err = max_e;

    if (sum_sq_err <= 1e-15) {
        snr_db = 99.0; // Perfect reconstruction
    } else {
        snr_db = 10.0 * std::log10(sum_sq_sig / sum_sq_err);
    }
}

} // namespace hydrogen
