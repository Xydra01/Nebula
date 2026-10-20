// hydrogen/src/memory/page_pool.cpp
// Lock-free atomic bitmask page pool allocator and VRAM compaction sweeper

#include "page_pool.hpp"
#include <algorithm>
#include <cassert>

namespace hydrogen {

PagePool::PagePool(PagePoolConfig config)
    : total_pages_(config.total_pages),
      page_size_bytes_(config.page_size_bytes),
      num_words_((config.total_pages + 63) / 64),
      bitmap_(num_words_) {
    assert(total_pages_ > 0);

    for (size_t i = 0; i < num_words_; ++i) {
        if (i == num_words_ - 1 && (total_pages_ % 64 != 0)) {
            // Last word: set only valid bits 0..(total_pages_ % 64 - 1) to 1 (FREE)
            uint64_t mask = (1ULL << (total_pages_ % 64)) - 1;
            bitmap_[i].store(mask, std::memory_order_relaxed);
        } else {
            // Full word: all 64 bits set to 1 (FREE)
            bitmap_[i].store(~0ULL, std::memory_order_relaxed);
        }
    }

    search_hint_.store(0, std::memory_order_relaxed);
    allocated_count_.store(0, std::memory_order_relaxed);
    double_free_errors_.store(0, std::memory_order_relaxed);
}

uint32_t PagePool::allocate_page() noexcept {
    if (allocated_count_.load(std::memory_order_relaxed) >= total_pages_) {
        return INVALID_PAGE_ID;
    }

    size_t start_idx = search_hint_.load(std::memory_order_relaxed) % num_words_;

    for (size_t step = 0; step < num_words_; ++step) {
        size_t word_idx = (start_idx + step) % num_words_;
        uint64_t word = bitmap_[word_idx].load(std::memory_order_relaxed);

        while (word != 0) {
            int bit = std::countr_zero(word);
            uint64_t mask = 1ULL << bit;

            if (bitmap_[word_idx].compare_exchange_weak(
                    word, word & ~mask,
                    std::memory_order_acquire,
                    std::memory_order_relaxed)) {
                // Successfully cleared bit (claimed free page)
                uint32_t page_id = static_cast<uint32_t>(word_idx * 64 + bit);
                if (page_id < total_pages_) {
                    allocated_count_.fetch_add(1, std::memory_order_relaxed);
                    search_hint_.store(word_idx, std::memory_order_relaxed);
                    return page_id;
                } else {
                    // Out-of-bounds boundary bit in last word; undo and abort this word
                    bitmap_[word_idx].fetch_or(mask, std::memory_order_release);
                    break;
                }
            }
            // CAS failed: word reloaded by compare_exchange_weak, continue inner loop
        }
    }

    return INVALID_PAGE_ID;
}

size_t PagePool::allocate_batch(size_t count, std::vector<uint32_t>& out_pages) {
    if (count == 0 || allocated_count_.load(std::memory_order_relaxed) >= total_pages_) {
        return 0;
    }

    out_pages.reserve(out_pages.size() + count);
    size_t allocated = 0;
    size_t start_idx = search_hint_.load(std::memory_order_relaxed) % num_words_;

    for (size_t step = 0; step < num_words_ && allocated < count; ++step) {
        size_t word_idx = (start_idx + step) % num_words_;
        uint64_t word = bitmap_[word_idx].load(std::memory_order_relaxed);

        while (word != 0 && allocated < count) {
            uint64_t claim_mask = 0;
            uint64_t temp = word;
            size_t bits_to_claim = 0;
            size_t max_bits = std::min(count - allocated, size_t(64));
            uint32_t claimed_pids[64];

            while (temp != 0 && bits_to_claim < max_bits) {
                int bit = std::countr_zero(temp);
                uint64_t m = 1ULL << bit;
                uint32_t pid = static_cast<uint32_t>(word_idx * 64 + bit);
                if (pid < total_pages_) {
                    claim_mask |= m;
                    claimed_pids[bits_to_claim++] = pid;
                }
                temp &= ~m;
            }

            if (claim_mask == 0) {
                break;
            }

            if (bitmap_[word_idx].compare_exchange_weak(
                    word, word & ~claim_mask,
                    std::memory_order_acquire,
                    std::memory_order_relaxed)) {
                for (size_t b = 0; b < bits_to_claim; ++b) {
                    out_pages.push_back(claimed_pids[b]);
                }
                allocated += bits_to_claim;
                allocated_count_.fetch_add(bits_to_claim, std::memory_order_relaxed);
                if ((word & ~claim_mask) == 0) {
                    search_hint_.store((word_idx + 1) % num_words_, std::memory_order_relaxed);
                } else {
                    search_hint_.store(word_idx, std::memory_order_relaxed);
                }
                break;
            }
        }
    }
    return allocated;
}

bool PagePool::free_page(uint32_t page_id) noexcept {
    if (page_id >= total_pages_) {
        return false;
    }

    size_t word_idx = page_id / 64;
    int bit = page_id % 64;
    uint64_t mask = 1ULL << bit;

    uint64_t prev = bitmap_[word_idx].fetch_or(mask, std::memory_order_release);
    if ((prev & mask) != 0) {
        // Bit was already 1 (FREE) -> Illegal Double Free detected!
        double_free_errors_.fetch_add(1, std::memory_order_relaxed);
        return false;
    }

    allocated_count_.fetch_sub(1, std::memory_order_relaxed);
    search_hint_.store(word_idx, std::memory_order_relaxed);
    return true;
}

size_t PagePool::free_batch(std::span<const uint32_t> page_ids) noexcept {
    size_t freed = 0;
    for (uint32_t pid : page_ids) {
        if (free_page(pid)) {
            freed++;
        }
    }
    return freed;
}

bool PagePool::is_page_allocated(uint32_t page_id) const noexcept {
    if (page_id >= total_pages_) {
        return false;
    }
    size_t word_idx = page_id / 64;
    int bit = page_id % 64;
    uint64_t mask = 1ULL << bit;
    uint64_t word = bitmap_[word_idx].load(std::memory_order_relaxed);
    return (word & mask) == 0; // 0 = allocated
}

double PagePool::calculate_fragmentation(
    size_t& out_largest_free_run,
    size_t& out_free_holes
) const noexcept {
    size_t total_free = free_pages_count();
    out_largest_free_run = 0;
    out_free_holes = 0;

    if (total_free == 0 || total_free == total_pages_) {
        out_largest_free_run = total_free;
        out_free_holes = (total_free == total_pages_) ? 1 : 0;
        return 0.0;
    }

    size_t current_run = 0;
    bool in_free_run = false;

    for (size_t i = 0; i < total_pages_; ++i) {
        bool free = !is_page_allocated(static_cast<uint32_t>(i));
        if (free) {
            current_run++;
            if (!in_free_run) {
                in_free_run = true;
                out_free_holes++;
            }
            if (current_run > out_largest_free_run) {
                out_largest_free_run = current_run;
            }
        } else {
            current_run = 0;
            in_free_run = false;
        }
    }

    return 1.0 - (static_cast<double>(out_largest_free_run) / static_cast<double>(total_free));
}

size_t PagePool::compact_pool(std::vector<std::pair<uint32_t, uint32_t>>& page_relocations) {
    page_relocations.clear();

    if (allocated_count_.load(std::memory_order_relaxed) == 0) {
        return 0;
    }

    int64_t left = 0;
    int64_t right = static_cast<int64_t>(total_pages_ - 1);

    while (left < right) {
        // Advance left until a free slot (hole) is found
        while (left < right && is_page_allocated(static_cast<uint32_t>(left))) {
            left++;
        }
        // Advance right backwards until an allocated slot is found
        while (left < right && !is_page_allocated(static_cast<uint32_t>(right))) {
            right--;
        }

        if (left < right) {
            uint32_t from_pid = static_cast<uint32_t>(right);
            uint32_t to_pid = static_cast<uint32_t>(left);

            page_relocations.emplace_back(from_pid, to_pid);

            // Execute relocation in bitmap
            // Claim to_pid (0 = allocated)
            size_t to_word = to_pid / 64;
            uint64_t to_mask = 1ULL << (to_pid % 64);
            bitmap_[to_word].fetch_and(~to_mask, std::memory_order_relaxed);

            // Free from_pid (1 = free)
            size_t from_word = from_pid / 64;
            uint64_t from_mask = 1ULL << (from_pid % 64);
            bitmap_[from_word].fetch_or(from_mask, std::memory_order_relaxed);

            left++;
            right--;
        }
    }

    return page_relocations.size();
}

} // namespace hydrogen
