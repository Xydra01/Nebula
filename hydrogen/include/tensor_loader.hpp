#pragma once

#include "gguf_parser.hpp"
#include "mapped_file.hpp"
#include <filesystem>
#include <memory>
#include <string>

namespace hydrogen {

/**
 * @brief Zero-copy memory-mapped GGUF model loader.
 */
class ModelLoader {
public:
    ModelLoader() = default;
    ~ModelLoader() = default;

    // Non-copyable, movable
    ModelLoader(const ModelLoader&) = delete;
    ModelLoader& operator=(const ModelLoader&) = delete;
    ModelLoader(ModelLoader&&) noexcept = default;
    ModelLoader& operator=(ModelLoader&&) noexcept = default;

    /**
     * @brief Open a GGUF file from disk, memory-map it, and parse headers.
     *
     * @param path Path to .gguf model file
     * @return true if successfully mapped and parsed, false or throws on error
     */
    void load(const std::filesystem::path& path);

    /**
     * @brief Close and unmap current model.
     */
    void close() noexcept;

    [[nodiscard]] bool is_loaded() const noexcept {
        return mapped_file_.is_open();
    }

    [[nodiscard]] const GGUFModel& model() const noexcept {
        return model_;
    }

    [[nodiscard]] const MappedFile& mapped_file() const noexcept {
        return mapped_file_;
    }

private:
    MappedFile mapped_file_;
    GGUFModel model_;
};

} // namespace hydrogen
