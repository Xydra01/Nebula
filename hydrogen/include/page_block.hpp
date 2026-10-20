#pragma once

#include <cstdint>
#include <cstddef>
#include <array>
#include <span>
#include <string>
#include <vector>

namespace hydrogen {

/**
 * @brief Quantization type tag for uniform KV cache page blocks.
 */
enum class PageQuantType : uint8_t {
    FP8_E4M3 = 0,
    Q8_0     = 1,
    FP16     = 2
};

/**
 * @brief Constants for UniformPageBlock geometry.
 */
inline constexpr size_t PAGE_BLOCK_TOKENS = 16;
inline constexpr size_t PAGE_BLOCK_DATA_BYTES = 1280; // 80 bytes per token * 16 tokens
inline constexpr size_t PAGE_BLOCK_TOTAL_BYTES = 1288; // 1 + 3 + 4 + 1280

/**
 * @brief Fixed-size, uniform 1,288-byte container for 16-token KV cache page blocks.
 *
 * Encapsulates dual FP8 (E4M3) and Q8_0 quantizations without heap fragmentation.
 * Memory layout is strictly aligned and deterministic across all layers.
 */
#pragma pack(push, 1)
struct UniformPageBlock {
    uint8_t quant_type = 0;              // 0 = FP8_E4M3, 1 = Q8_0, 2 = FP16
    uint8_t reserved[3] = {0, 0, 0};     // Alignment padding to 4-byte boundary
    float dynamic_scale = 1.0f;          // Per-page dynamic dequantization scale
    uint8_t data[PAGE_BLOCK_DATA_BYTES]; // 1,280-byte uniform data payload
};
#pragma pack(pop)

static_assert(sizeof(UniformPageBlock) == PAGE_BLOCK_TOTAL_BYTES,
              "UniformPageBlock size must be exactly 1,288 bytes to prevent heap fragmentation");
static_assert(offsetof(UniformPageBlock, quant_type) == 0, "quant_type must be at offset 0");
static_assert(offsetof(UniformPageBlock, reserved) == 1, "reserved must be at offset 1");
static_assert(offsetof(UniformPageBlock, dynamic_scale) == 4, "dynamic_scale must be at offset 4");
static_assert(offsetof(UniformPageBlock, data) == 8, "data must be at offset 8");

/**
 * @brief FP8 (E4M3) conversion utilities.
 */
class FP8_E4M3 {
public:
    static uint8_t from_float(float val);
    static float to_float(uint8_t byte);

    // Precomputed lookup table for instantaneous dequantization
    static const std::array<float, 256>& dequant_lut();
};

/**
 * @brief Q8_0 symmetric integer conversion utilities.
 */
class Q8_0 {
public:
    static int8_t from_float(float val, float scale);
    static float to_float(int8_t byte, float scale);
};

/**
 * @brief Policy determining whether a layer should use FP8 or Q8_0.
 */
class PageQuantPolicy {
public:
    explicit PageQuantPolicy(uint32_t total_layers, uint32_t sensitive_boundary = 4);

    /**
     * @brief Determine quantization type for a given layer index.
     *
     * Initial boundary layers (0..3) and final boundary layers (L-4..L-1)
     * use Q8_0 to prevent code syntax drift. Intermediate layers use FP8.
     */
    [[nodiscard]] PageQuantType get_layer_quant_type(uint32_t layer_idx) const noexcept;

    /**
     * @brief Check if kurtosis / activation outlier ratio demands Q8_0 fallback.
     */
    [[nodiscard]] PageQuantType evaluate_activation_kurtosis(uint32_t layer_idx, float kurtosis) const noexcept;

    [[nodiscard]] uint32_t total_layers() const noexcept { return total_layers_; }
    [[nodiscard]] uint32_t sensitive_boundary() const noexcept { return sensitive_boundary_; }

private:
    uint32_t total_layers_;
    uint32_t sensitive_boundary_;
};

/**
 * @brief Page Encoder and Decoder interface.
 */
class PageBlockCodec {
public:
    /**
     * @brief Encode 1,280 float values into a UniformPageBlock using given quantization type.
     */
    static void encode(
        std::span<const float> src_floats,
        UniformPageBlock& dst_block,
        PageQuantType quant_type
    );

    /**
     * @brief Decode a UniformPageBlock back into 1,280 float values.
     */
    static void decode(
        const UniformPageBlock& src_block,
        std::span<float> dst_floats
    );

    /**
     * @brief Measure Mean Squared Error (MSE) and Signal-to-Noise Ratio (SNR) of roundtrip.
     */
    static void measure_fidelity(
        std::span<const float> original,
        std::span<const float> reconstructed,
        double& mse,
        double& max_err,
        double& snr_db
    );
};

} // namespace hydrogen
