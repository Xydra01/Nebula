#include "tensor_loader.hpp"
#include <algorithm>
#include <iostream>

namespace hydrogen {

namespace {

class BinaryReader {
public:
    BinaryReader(const uint8_t* data, size_t size)
        : data_(data), size_(size), cursor_(0) {}

    template <typename T>
    T read_pod() {
        if (cursor_ + sizeof(T) > size_) {
            throw std::runtime_error("Unexpected end of GGUF buffer at offset " + 
                                     std::to_string(cursor_));
        }
        T val;
        std::memcpy(&val, data_ + cursor_, sizeof(T));
        cursor_ += sizeof(T);
        return val;
    }

    std::string read_string() {
        uint64_t len = read_pod<uint64_t>();
        if (cursor_ + len > size_) {
            throw std::runtime_error("GGUF string exceeds buffer at offset " + 
                                     std::to_string(cursor_));
        }
        std::string s(reinterpret_cast<const char*>(data_ + cursor_), static_cast<size_t>(len));
        cursor_ += static_cast<size_t>(len);
        return s;
    }

    GGUFValue read_value(GGUFType type) {
        GGUFValue val;
        val.type = type;
        switch (type) {
            case GGUFType::UINT8:
                val.value = read_pod<uint8_t>();
                break;
            case GGUFType::INT8:
                val.value = read_pod<int8_t>();
                break;
            case GGUFType::UINT16:
                val.value = read_pod<uint16_t>();
                break;
            case GGUFType::INT16:
                val.value = read_pod<int16_t>();
                break;
            case GGUFType::UINT32:
                val.value = read_pod<uint32_t>();
                break;
            case GGUFType::INT32:
                val.value = read_pod<int32_t>();
                break;
            case GGUFType::FLOAT32:
                val.value = read_pod<float>();
                break;
            case GGUFType::BOOL:
                val.value = (read_pod<uint8_t>() != 0);
                break;
            case GGUFType::STRING:
                val.value = read_string();
                break;
            case GGUFType::ARRAY: {
                uint32_t element_type_raw = read_pod<uint32_t>();
                uint64_t count = read_pod<uint64_t>();
                GGUFType element_type = static_cast<GGUFType>(element_type_raw);
                std::vector<GGUFValue> arr;
                arr.reserve(std::min<size_t>(static_cast<size_t>(count), 1000000));
                for (uint64_t i = 0; i < count; ++i) {
                    arr.push_back(read_value(element_type));
                }
                val.value = std::move(arr);
                break;
            }
            case GGUFType::UINT64:
                val.value = read_pod<uint64_t>();
                break;
            case GGUFType::INT64:
                val.value = read_pod<int64_t>();
                break;
            case GGUFType::FLOAT64:
                val.value = read_pod<double>();
                break;
            default:
                throw std::runtime_error("Unknown GGUF value type: " + 
                                         std::to_string(static_cast<uint32_t>(type)));
        }
        return val;
    }

    [[nodiscard]] size_t cursor() const noexcept { return cursor_; }

private:
    const uint8_t* data_;
    size_t size_;
    size_t cursor_;
};

} // anonymous namespace

GGUFModel parse_gguf(const uint8_t* buffer, size_t buffer_size) {
    if (buffer == nullptr || buffer_size < 24) {
        throw std::runtime_error("Invalid or truncated GGUF buffer (size < 24 bytes)");
    }

    BinaryReader reader(buffer, buffer_size);
    uint32_t magic = reader.read_pod<uint32_t>();
    if (magic != GGUF_MAGIC) {
        throw std::runtime_error("Invalid GGUF magic identifier (expected 'GGUF')");
    }

    GGUFModel model;
    model.version = reader.read_pod<uint32_t>();
    if (model.version < 2 || model.version > 3) {
        throw std::runtime_error("Unsupported GGUF version: " + std::to_string(model.version));
    }

    model.tensor_count = reader.read_pod<uint64_t>();
    model.kv_count = reader.read_pod<uint64_t>();

    // Parse metadata
    for (uint64_t i = 0; i < model.kv_count; ++i) {
        std::string key = reader.read_string();
        uint32_t val_type_raw = reader.read_pod<uint32_t>();
        GGUFValue val = reader.read_value(static_cast<GGUFType>(val_type_raw));
        model.metadata[std::move(key)] = std::move(val);
    }

    // Alignment from metadata
    model.alignment = model.get_uint64("general.alignment", 32);
    if (model.alignment == 0) {
        model.alignment = 32;
    }

    // Parse tensor info
    model.tensors.reserve(static_cast<size_t>(model.tensor_count));
    for (uint64_t i = 0; i < model.tensor_count; ++i) {
        GGUFTensor tensor;
        tensor.name = reader.read_string();
        uint32_t n_dims = reader.read_pod<uint32_t>();
        if (n_dims > 4) {
            throw std::runtime_error("Unsupported tensor dimensions (" + 
                                     std::to_string(n_dims) + ") for tensor: " + tensor.name);
        }
        tensor.shape.resize(n_dims);
        for (uint32_t d = 0; d < n_dims; ++d) {
            tensor.shape[d] = reader.read_pod<uint64_t>();
        }
        tensor.type = reader.read_pod<uint32_t>();
        tensor.offset = reader.read_pod<uint64_t>();

        // Calculate size in bytes
        size_t block_size = 1;
        size_t block_bytes = 1;
        ggml_type_traits(tensor.type, block_size, block_bytes);
        uint64_t elements = tensor.num_elements();
        if (block_size > 0) {
            tensor.size_bytes = (elements / block_size) * block_bytes;
        } else {
            tensor.size_bytes = elements * block_bytes;
        }

        model.tensors.push_back(std::move(tensor));
    }

    // Align tensor data section
    size_t header_end = reader.cursor();
    model.header_size_bytes = header_end;
    model.tensor_data_offset = (header_end + model.alignment - 1) / model.alignment * model.alignment;

    // Resolve pointers and total tensor bytes
    uint64_t max_tensor_extent = 0;
    for (size_t i = 0; i < model.tensors.size(); ++i) {
        auto& t = model.tensors[i];
        t.absolute_offset = model.tensor_data_offset + t.offset;
        if (t.absolute_offset + t.size_bytes <= buffer_size) {
            t.data = buffer + t.absolute_offset;
        } else {
            t.data = nullptr;
        }
        model.tensor_index_by_name[t.name] = i;

        uint64_t extent = t.offset + t.size_bytes;
        if (extent > max_tensor_extent) {
            max_tensor_extent = extent;
        }
    }
    model.total_tensor_bytes = max_tensor_extent;

    return model;
}

void ModelLoader::load(const std::filesystem::path& path) {
    mapped_file_.open(path);
    model_ = parse_gguf(mapped_file_.data(), mapped_file_.size());
}

void ModelLoader::close() noexcept {
    model_ = GGUFModel{};
    mapped_file_.close();
}

} // namespace hydrogen
