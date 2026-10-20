#pragma once

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif

#include <windows.h>
#include <cstdint>
#include <filesystem>
#include <stdexcept>
#include <string>

namespace hydrogen {

/**
 * @brief High-performance Win32 RAII memory-mapped file wrapper.
 *
 * Provides zero-copy read-only access to large GGUF model files
 * mapped directly into 64-bit virtual address space.
 */
class MappedFile {
public:
    MappedFile() noexcept
        : file_handle_(INVALID_HANDLE_VALUE),
          mapping_handle_(NULL),
          mapped_ptr_(nullptr),
          file_size_(0) {}

    ~MappedFile() {
        close();
    }

    // Non-copyable
    MappedFile(const MappedFile&) = delete;
    MappedFile& operator=(const MappedFile&) = delete;

    // Movable
    MappedFile(MappedFile&& other) noexcept
        : file_handle_(other.file_handle_),
          mapping_handle_(other.mapping_handle_),
          mapped_ptr_(other.mapped_ptr_),
          file_size_(other.file_size_),
          path_(std::move(other.path_)) {
        other.file_handle_ = INVALID_HANDLE_VALUE;
        other.mapping_handle_ = NULL;
        other.mapped_ptr_ = nullptr;
        other.file_size_ = 0;
    }

    MappedFile& operator=(MappedFile&& other) noexcept {
        if (this != &other) {
            close();
            file_handle_ = other.file_handle_;
            mapping_handle_ = other.mapping_handle_;
            mapped_ptr_ = other.mapped_ptr_;
            file_size_ = other.file_size_;
            path_ = std::move(other.path_);

            other.file_handle_ = INVALID_HANDLE_VALUE;
            other.mapping_handle_ = NULL;
            other.mapped_ptr_ = nullptr;
            other.file_size_ = 0;
        }
        return *this;
    }

    /**
     * @brief Memory-map a file from disk with sequential scan hint.
     */
    void open(const std::filesystem::path& path) {
        close();
        path_ = path;

        std::wstring wpath = path.wstring();

        // 1. Open file with sequential scan optimization flag
        file_handle_ = CreateFileW(
            wpath.c_str(),
            GENERIC_READ,
            FILE_SHARE_READ,
            NULL,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN,
            NULL
        );

        if (file_handle_ == INVALID_HANDLE_VALUE) {
            DWORD err = GetLastError();
            throw std::runtime_error("Failed to open file '" + path.string() + 
                                     "': Win32 error " + std::to_string(err));
        }

        // 2. Query file size
        LARGE_INTEGER size;
        if (!GetFileSizeEx(file_handle_, &size)) {
            DWORD err = GetLastError();
            close();
            throw std::runtime_error("Failed to get file size for '" + path.string() + 
                                     "': Win32 error " + std::to_string(err));
        }
        file_size_ = static_cast<uint64_t>(size.QuadPart);

        if (file_size_ == 0) {
            // Cannot map an empty file
            close();
            throw std::runtime_error("File '" + path.string() + "' is empty (0 bytes)");
        }

        // 3. Create read-only file mapping object
        mapping_handle_ = CreateFileMappingW(
            file_handle_,
            NULL,
            PAGE_READONLY,
            0,
            0,
            NULL
        );

        if (mapping_handle_ == NULL) {
            DWORD err = GetLastError();
            close();
            throw std::runtime_error("CreateFileMappingW failed for '" + path.string() + 
                                     "': Win32 error " + std::to_string(err));
        }

        // 4. Map entire file view into address space
        mapped_ptr_ = MapViewOfFile(
            mapping_handle_,
            FILE_MAP_READ,
            0,
            0,
            0 // map whole file
        );

        if (mapped_ptr_ == nullptr) {
            DWORD err = GetLastError();
            close();
            throw std::runtime_error("MapViewOfFile failed for '" + path.string() + 
                                     "': Win32 error " + std::to_string(err));
        }
    }

    /**
     * @brief Unmap and close handles.
     */
    void close() noexcept {
        if (mapped_ptr_ != nullptr) {
            UnmapViewOfFile(mapped_ptr_);
            mapped_ptr_ = nullptr;
        }
        if (mapping_handle_ != NULL) {
            CloseHandle(mapping_handle_);
            mapping_handle_ = NULL;
        }
        if (file_handle_ != INVALID_HANDLE_VALUE) {
            CloseHandle(file_handle_);
            file_handle_ = INVALID_HANDLE_VALUE;
        }
        file_size_ = 0;
    }

    [[nodiscard]] bool is_open() const noexcept {
        return mapped_ptr_ != nullptr;
    }

    [[nodiscard]] const uint8_t* data() const noexcept {
        return static_cast<const uint8_t*>(mapped_ptr_);
    }

    [[nodiscard]] uint64_t size() const noexcept {
        return file_size_;
    }

    [[nodiscard]] const std::filesystem::path& path() const noexcept {
        return path_;
    }

private:
    HANDLE file_handle_;
    HANDLE mapping_handle_;
    void* mapped_ptr_;
    uint64_t file_size_;
    std::filesystem::path path_;
};

} // namespace hydrogen
