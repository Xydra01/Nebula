#pragma once

#include "page_block.hpp"
#include <atomic>
#include <bit>
#include <cstdint>
#include <cstddef>
#include <vector>
#include <span>
#include <utility>

namespace hydrogen {

/**
 * @brief Sentinel value indicating allocation failure or invalid page ID.
 */
inline constexpr uint32_t INVALID_PAGE_ID = 0xFFFFFFFF;

/**
 * @brief Configuration parameters for the Lock-Free Page Pool.
 */
struct PagePoolConfig {
    size_t total_pages = 65536;                         // Default test pool capacity
    size_t page_size_bytes = PAGE_BLOCK_TOTAL_BYTES;    // 1,288 bytes per UniformPageBlock
};

/**
 * @brief Lock-Free Atomic Bitmask Page Pool Allocator with VRAM Compaction Sweeper.
 *
 * Implements non-blocking, wait-free/lock-free page allocation and release using
 * 64-bit atomic bitmasks and hardware bit scan intrinsics (std::countr_zero / _tzcnt_u64).
 */
class PagePool {
public:
    explicit PagePool(PagePoolConfig config);
    ~PagePool() = default;

    // Non-copyable, non-movable
    PagePool(const PagePool&) = delete;
    PagePool& operator=(const PagePool&) = delete;
    PagePool(PagePool&&) = delete;
    PagePool& operator=(PagePool&&) = delete;

    /**
     * @brief Allocate a single physical page block lock-free.
     * @return Physical page ID, or INVALID_PAGE_ID if pool is exhausted.
     */
    [[nodiscard]] uint32_t allocate_page() noexcept;

    /**
     * @brief Allocate up to `count` physical page blocks lock-free.
     * @param count Target number of pages to allocate.
     * @param out_pages Output vector where allocated page IDs are appended.
     * @return Number of pages successfully allocated.
     */
    size_t allocate_batch(size_t count, std::vector<uint32_t>& out_pages);

    /**
     * @brief Release a previously allocated physical page block.
     * @param page_id The physical page ID to free.
     * @return true if successfully returned to pool, false if double-free or out of bounds.
     */
    bool free_page(uint32_t page_id) noexcept;

    /**
     * @brief Release a batch of physical page blocks.
     * @param page_ids Span of page IDs to free.
     * @return Number of pages successfully returned to pool without error.
     */
    size_t free_batch(std::span<const uint32_t> page_ids) noexcept;

    /**
     * @brief Check whether a specific page ID is currently allocated.
     */
    [[nodiscard]] bool is_page_allocated(uint32_t page_id) const noexcept;

    /**
     * @brief Current number of active allocated pages.
     */
    [[nodiscard]] size_t allocated_pages_count() const noexcept {
        return allocated_count_.load(std::memory_order_relaxed);
    }

    /**
     * @brief Current number of available free pages.
     */
    [[nodiscard]] size_t free_pages_count() const noexcept {
        return total_pages_ - allocated_pages_count();
    }

    /**
     * @brief Total capacity in physical pages.
     */
    [[nodiscard]] size_t capacity() const noexcept {
        return total_pages_;
    }

    /**
     * @brief Size of each physical page in bytes.
     */
    [[nodiscard]] size_t page_size_bytes() const noexcept {
        return page_size_bytes_;
    }

    /**
     * @brief Total physical memory managed by this pool in bytes.
     */
    [[nodiscard]] size_t total_memory_bytes() const noexcept {
        return total_pages_ * page_size_bytes_;
    }

    /**
     * @brief Calculate VRAM fragmentation metrics.
     * @param out_largest_free_run Outputs the maximum contiguous block of free pages.
     * @param out_free_holes Outputs the number of disjoint free regions.
     * @return Fragmentation ratio between 0.0 (perfectly contiguous) and 1.0 (highly fragmented).
     */
    [[nodiscard]] double calculate_fragmentation(
        size_t& out_largest_free_run,
        size_t& out_free_holes
    ) const noexcept;

    /**
     * @brief Compact memory pool by identifying disparate leaf pages and planning relocations to free holes.
     * @param page_relocations Output vector of pairs {from_page_id, to_page_id} describing the relocation plan.
     * @return Number of pages compacted.
     */
    size_t compact_pool(std::vector<std::pair<uint32_t, uint32_t>>& page_relocations);

private:
    size_t total_pages_;
    size_t page_size_bytes_;
    size_t num_words_;

    // Array of 64-bit atomic words. Bit 1 = FREE, Bit 0 = ALLOCATED.
    std::vector<std::atomic<uint64_t>> bitmap_;

    // Hint cursor for lock-free allocation locality
    std::atomic<size_t> search_hint_{0};

    // Tracking atomic counters
    std::atomic<size_t> allocated_count_{0};
    std::atomic<size_t> double_free_errors_{0};
};

} // namespace hydrogen
