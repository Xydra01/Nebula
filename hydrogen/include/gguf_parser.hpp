#pragma once

#include <cstdint>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_map>
#include <variant>
#include <vector>

namespace hydrogen {

constexpr uint32_t GGUF_MAGIC = 0x46554747; // "GGUF" in little-endian

enum class GGUFType : uint32_t {
    UINT8 = 0,
    INT8 = 1,
    UINT16 = 2,
    INT16 = 3,
    UINT32 = 4,
    INT32 = 5,
    FLOAT32 = 6,
    BOOL = 7,
    STRING = 8,
    ARRAY = 9,
    UINT64 = 10,
    INT64 = 11,
    FLOAT64 = 12,
};

enum class GGMLType : uint32_t {
    F32 = 0,
    F16 = 1,
    Q4_0 = 2,
    Q4_1 = 3,
    Q5_0 = 6,
    Q5_1 = 7,
    Q8_0 = 8,
    Q8_1 = 9,
    Q2_K = 10,
    Q3_K = 11,
    Q4_K = 12,
    Q5_K = 13,
    Q6_K = 14,
    Q8_K = 15,
    IQ2_XXS = 16,
    IQ2_XS = 17,
    IQ3_XXS = 18,
    IQ1_S = 19,
    IQ4_NL = 20,
    IQ3_S = 21,
    IQ2_S = 22,
    IQ4_XS = 23,
    I8 = 24,
    I16 = 25,
    I32 = 26,
    I64 = 27,
    F64 = 28,
    IQ1_M = 29,
    BF16 = 30,
    TQ1_0 = 34,
    TQ2_0 = 35,
};

inline std::string ggml_type_name(uint32_t type) {
    switch (static_cast<GGMLType>(type)) {
        case GGMLType::F32: return "F32";
        case GGMLType::F16: return "F16";
        case GGMLType::Q4_0: return "Q4_0";
        case GGMLType::Q4_1: return "Q4_1";
        case GGMLType::Q5_0: return "Q5_0";
        case GGMLType::Q5_1: return "Q5_1";
        case GGMLType::Q8_0: return "Q8_0";
        case GGMLType::Q8_1: return "Q8_1";
        case GGMLType::Q2_K: return "Q2_K";
        case GGMLType::Q3_K: return "Q3_K";
        case GGMLType::Q4_K: return "Q4_K";
        case GGMLType::Q5_K: return "Q5_K";
        case GGMLType::Q6_K: return "Q6_K";
        case GGMLType::Q8_K: return "Q8_K";
        case GGMLType::IQ2_XXS: return "IQ2_XXS";
        case GGMLType::IQ2_XS: return "IQ2_XS";
        case GGMLType::IQ3_XXS: return "IQ3_XXS";
        case GGMLType::IQ1_S: return "IQ1_S";
        case GGMLType::IQ4_NL: return "IQ4_NL";
        case GGMLType::IQ3_S: return "IQ3_S";
        case GGMLType::IQ2_S: return "IQ2_S";
        case GGMLType::IQ4_XS: return "IQ4_XS";
        case GGMLType::I8: return "I8";
        case GGMLType::I16: return "I16";
        case GGMLType::I32: return "I32";
        case GGMLType::I64: return "I64";
        case GGMLType::F64: return "F64";
        case GGMLType::IQ1_M: return "IQ1_M";
        case GGMLType::BF16: return "BF16";
        case GGMLType::TQ1_0: return "TQ1_0";
        case GGMLType::TQ2_0: return "TQ2_0";
        default: {
            if (type == 142) return "PQ2_0";
            if (type == 143) return "TQ1_0";
            if (type == 144) return "TQ2_0";
            return "UNKNOWN(" + std::to_string(type) + ")";
        }
    }
}

/**
 * @brief Block dimensions and byte sizes for GGML tensor calculation.
 */
inline void ggml_type_traits(uint32_t type, size_t& block_size, size_t& block_bytes) {
    switch (static_cast<GGMLType>(type)) {
        case GGMLType::F32: block_size = 1; block_bytes = 4; break;
        case GGMLType::F16: block_size = 1; block_bytes = 2; break;
        case GGMLType::BF16: block_size = 1; block_bytes = 2; break;
        case GGMLType::I8: block_size = 1; block_bytes = 1; break;
        case GGMLType::I16: block_size = 1; block_bytes = 2; break;
        case GGMLType::I32: block_size = 1; block_bytes = 4; break;
        case GGMLType::I64: block_size = 1; block_bytes = 8; break;
        case GGMLType::F64: block_size = 1; block_bytes = 8; break;
        case GGMLType::Q4_0: block_size = 32; block_bytes = 18; break;
        case GGMLType::Q4_1: block_size = 32; block_bytes = 20; break;
        case GGMLType::Q5_0: block_size = 32; block_bytes = 22; break;
        case GGMLType::Q5_1: block_size = 32; block_bytes = 24; break;
        case GGMLType::Q8_0: block_size = 32; block_bytes = 34; break;
        case GGMLType::Q8_1: block_size = 32; block_bytes = 40; break;
        case GGMLType::Q2_K: block_size = 256; block_bytes = 84; break;
        case GGMLType::Q3_K: block_size = 256; block_bytes = 110; break;
        case GGMLType::Q4_K: block_size = 256; block_bytes = 144; break;
        case GGMLType::Q5_K: block_size = 256; block_bytes = 176; break;
        case GGMLType::Q6_K: block_size = 256; block_bytes = 210; break;
        case GGMLType::Q8_K: block_size = 256; block_bytes = 292; break;
        case GGMLType::IQ4_NL: block_size = 32; block_bytes = 18; break;
        case GGMLType::TQ1_0: block_size = 256; block_bytes = 56; break;
        case GGMLType::TQ2_0: block_size = 256; block_bytes = 70; break;
        default:
            if (type == 142) { block_size = 256; block_bytes = 70; break; }
            if (type == 143) { block_size = 256; block_bytes = 56; break; }
            // Fallback for unlisted quantization types
            block_size = 1;
            block_bytes = 1;
            break;
    }
}

// Forward declare for recursive array value definition
struct GGUFValue;

using GGUFValueVariant = std::variant<
    uint8_t,
    int8_t,
    uint16_t,
    int16_t,
    uint32_t,
    int32_t,
    float,
    bool,
    std::string,
    std::vector<GGUFValue>,
    uint64_t,
    int64_t,
    double
>;

struct GGUFValue {
    GGUFType type;
    GGUFValueVariant value;

    [[nodiscard]] std::string as_string() const {
        if (std::holds_alternative<std::string>(value)) {
            return std::get<std::string>(value);
        }
        return {};
    }

    [[nodiscard]] uint64_t as_uint64() const {
        if (std::holds_alternative<uint64_t>(value)) return std::get<uint64_t>(value);
        if (std::holds_alternative<uint32_t>(value)) return std::get<uint32_t>(value);
        if (std::holds_alternative<uint16_t>(value)) return std::get<uint16_t>(value);
        if (std::holds_alternative<uint8_t>(value)) return std::get<uint8_t>(value);
        if (std::holds_alternative<int64_t>(value)) return static_cast<uint64_t>(std::get<int64_t>(value));
        if (std::holds_alternative<int32_t>(value)) return static_cast<uint64_t>(std::get<int32_t>(value));
        return 0;
    }

    [[nodiscard]] uint32_t as_uint32() const {
        return static_cast<uint32_t>(as_uint64());
    }

    [[nodiscard]] int64_t as_int64() const {
        if (std::holds_alternative<int64_t>(value)) return std::get<int64_t>(value);
        if (std::holds_alternative<int32_t>(value)) return std::get<int32_t>(value);
        if (std::holds_alternative<int16_t>(value)) return std::get<int16_t>(value);
        if (std::holds_alternative<int8_t>(value)) return std::get<int8_t>(value);
        if (std::holds_alternative<uint64_t>(value)) return static_cast<int64_t>(std::get<uint64_t>(value));
        if (std::holds_alternative<uint32_t>(value)) return static_cast<int64_t>(std::get<uint32_t>(value));
        return 0;
    }

    [[nodiscard]] int32_t as_int32() const {
        return static_cast<int32_t>(as_int64());
    }

    [[nodiscard]] float as_float() const {
        if (std::holds_alternative<float>(value)) return std::get<float>(value);
        if (std::holds_alternative<double>(value)) return static_cast<float>(std::get<double>(value));
        return 0.0f;
    }

    [[nodiscard]] bool as_bool() const {
        if (std::holds_alternative<bool>(value)) return std::get<bool>(value);
        if (std::holds_alternative<uint8_t>(value)) return std::get<uint8_t>(value) != 0;
        return false;
    }

    [[nodiscard]] const std::vector<GGUFValue>& as_array() const {
        static const std::vector<GGUFValue> empty;
        if (std::holds_alternative<std::vector<GGUFValue>>(value)) {
            return std::get<std::vector<GGUFValue>>(value);
        }
        return empty;
    }
};

struct GGUFTensor {
    std::string name;
    std::vector<uint64_t> shape;
    uint32_t type = 0;             // GGMLType enum
    uint64_t offset = 0;           // Relative to tensor data section
    uint64_t absolute_offset = 0;  // Absolute offset in file
    uint64_t size_bytes = 0;       // Computed byte size
    const uint8_t* data = nullptr; // Direct pointer into memory map

    [[nodiscard]] uint64_t num_elements() const noexcept {
        if (shape.empty()) return 0;
        uint64_t total = 1;
        for (uint64_t dim : shape) {
            total *= dim;
        }
        return total;
    }
};

struct GGUFModel {
    uint32_t version = 0;
    uint64_t tensor_count = 0;
    uint64_t kv_count = 0;
    uint64_t alignment = 32;
    uint64_t header_size_bytes = 0;
    uint64_t tensor_data_offset = 0;
    uint64_t total_tensor_bytes = 0;

    std::unordered_map<std::string, GGUFValue> metadata;
    std::vector<GGUFTensor> tensors;
    std::unordered_map<std::string, size_t> tensor_index_by_name;

    [[nodiscard]] std::string get_string(const std::string& key, const std::string& def = "") const {
        auto it = metadata.find(key);
        if (it != metadata.end()) {
            return it->second.as_string();
        }
        return def;
    }

    [[nodiscard]] uint64_t get_uint64(const std::string& key, uint64_t def = 0) const {
        auto it = metadata.find(key);
        if (it != metadata.end()) {
            return it->second.as_uint64();
        }
        return def;
    }

    [[nodiscard]] uint32_t get_uint32(const std::string& key, uint32_t def = 0) const {
        return static_cast<uint32_t>(get_uint64(key, def));
    }

    [[nodiscard]] int64_t get_int64(const std::string& key, int64_t def = 0) const {
        auto it = metadata.find(key);
        if (it != metadata.end()) {
            return it->second.as_int64();
        }
        return def;
    }

    [[nodiscard]] int32_t get_int32(const std::string& key, int32_t def = 0) const {
        return static_cast<int32_t>(get_int64(key, def));
    }

    [[nodiscard]] float get_float(const std::string& key, float def = 0.0f) const {
        auto it = metadata.find(key);
        if (it != metadata.end()) {
            return it->second.as_float();
        }
        return def;
    }

    [[nodiscard]] bool get_bool(const std::string& key, bool def = false) const {
        auto it = metadata.find(key);
        if (it != metadata.end()) {
            return it->second.as_bool();
        }
        return def;
    }

    [[nodiscard]] const GGUFTensor* find_tensor(const std::string& name) const {
        auto it = tensor_index_by_name.find(name);
        if (it != tensor_index_by_name.end() && it->second < tensors.size()) {
            return &tensors[it->second];
        }
        return nullptr;
    }

    // Common architecture accessors
    [[nodiscard]] std::string architecture() const {
        return get_string("general.architecture", "unknown");
    }

    [[nodiscard]] uint32_t context_length() const {
        std::string arch = architecture();
        return get_uint32(arch + ".context_length", get_uint32("general.context_length", 0));
    }

    [[nodiscard]] uint32_t embedding_length() const {
        std::string arch = architecture();
        return get_uint32(arch + ".embedding_length", 0);
    }

    [[nodiscard]] uint32_t layer_count() const {
        std::string arch = architecture();
        return get_uint32(arch + ".block_count", 0);
    }

    [[nodiscard]] uint32_t head_count() const {
        std::string arch = architecture();
        return get_uint32(arch + ".attention.head_count", 0);
    }

    [[nodiscard]] uint32_t head_count_kv() const {
        std::string arch = architecture();
        return get_uint32(arch + ".attention.head_count_kv", head_count());
    }
};

/**
 * @brief Parse a GGUF model directly from memory.
 *
 * @param buffer Pointer to the start of the memory-mapped file
 * @param buffer_size Size of the mapped file in bytes
 * @return GGUFModel Parsed metadata, tensor directory, and data pointers
 */
GGUFModel parse_gguf(const uint8_t* buffer, size_t buffer_size);

} // namespace hydrogen
