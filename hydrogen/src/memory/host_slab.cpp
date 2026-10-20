#include "host_slab.hpp"
#include <iostream>
#include <stdexcept>
#include <cstring>
#include <algorithm>

#ifdef _WIN32
#include <malloc.h>
#else
#include <stdlib.h>
#endif

// CUDA Runtime API for pinned host allocations
#include <cuda_runtime.h>

namespace hydrogen {

// ============================================================================
// SubArena Implementation
// ============================================================================

SubArena::SubArena()
    : bump_index(0), active_allocations(0), atomic_transactions(0),
      global_subarena_id(0), max_slots(SLOTS_PER_SUBARENA) {
    free_stack.reserve(4096);
}

uint32_t SubArena::allocate_batch(uint32_t* out_slots, uint32_t batch_size) {
    if (!out_slots || batch_size == 0) return 0;

    atomic_transactions.fetch_add(1, std::memory_order_relaxed);
    uint32_t allocated = 0;

    // 1. Try recycling freed slots first under lock
    {
        std::lock_guard<std::mutex> lock(free_lock);
        while (!free_stack.empty() && allocated < batch_size) {
            out_slots[allocated++] = free_stack.back();
            free_stack.pop_back();
        }
    }

    // 2. If batch not fulfilled, bump-allocate remaining slots
    if (allocated < batch_size) {
        uint32_t remaining = batch_size - allocated;
        uint32_t start_idx = bump_index.fetch_add(remaining, std::memory_order_relaxed);

        if (start_idx < max_slots) {
            uint32_t count = std::min(remaining, max_slots - start_idx);
            for (uint32_t i = 0; i < count; ++i) {
                uint32_t local_slot = start_idx + i;
                // Translate to global slot ID
                uint32_t slab_idx = global_subarena_id / SUBARENAS_PER_SLAB;
                uint32_t sub_in_slab = global_subarena_id % SUBARENAS_PER_SLAB;
                out_slots[allocated++] = slab_idx * SLOTS_PER_SLAB +
                                         sub_in_slab * SLOTS_PER_SUBARENA + local_slot;
            }
        }
    }

    active_allocations.fetch_add(allocated, std::memory_order_relaxed);
    return allocated;
}

void SubArena::free_batch(const uint32_t* in_slots, uint32_t batch_size) {
    if (!in_slots || batch_size == 0) return;

    atomic_transactions.fetch_add(1, std::memory_order_relaxed);
    {
        std::lock_guard<std::mutex> lock(free_lock);
        for (uint32_t i = 0; i < batch_size; ++i) {
            free_stack.push_back(in_slots[i]);
        }
    }
    active_allocations.fetch_sub(batch_size, std::memory_order_relaxed);
}

// ============================================================================
// TieredHostSlabAllocator Implementation
// ============================================================================

TieredHostSlabAllocator::TieredHostSlabAllocator(HostSlabConfig config)
    : config_(config), active_tier_(config.requested_tier) {
    
    // Cap memory strictly at 4 GiB
    size_t target_bytes = std::min(config_.custom_max_pinned_bytes, MAX_PINNED_BYTES);

    // Initialize 128 sub-arenas
    subarenas_ = std::make_unique<SubArena[]>(TOTAL_SUBARENAS);
    for (uint32_t i = 0; i < TOTAL_SUBARENAS; ++i) {
        subarenas_[i].global_subarena_id = i;
    }

    // ------------------------------------------------------------------------
    // Tiered Fallback Ladder
    // ------------------------------------------------------------------------
    bool allocated = false;

    if (config_.simulate_fragmentation) {
        if (config_.verbose) std::cout << "[HostSlab] [SIMULATION] Simulating kernel memory fragmentation on Tier 1 (1 x 4 GB)...\n";
        if (config_.force_tier3_fragmentation) {
            if (config_.verbose) std::cout << "[HostSlab] [SIMULATION] Simulating fragmentation failure on Tier 2 (4 x 1 GB); cascading to Tier 3 (16 x 256 MB)...\n";
            allocated = try_allocate_tier3(target_bytes);
            if (allocated) active_tier_ = AllocationTier::Tier3_Sixteen256MB;
        } else {
            if (config_.verbose) std::cout << "[HostSlab] [SIMULATION] Falling back to Tier 2 (4 x 1 GB)...\n";
            allocated = try_allocate_tier2(target_bytes);
            if (allocated) {
                active_tier_ = AllocationTier::Tier2_Four1GB;
            } else {
                if (config_.verbose) std::cout << "[HostSlab] Tier 2 failed; falling back to Tier 3 (16 x 256 MB)...\n";
                allocated = try_allocate_tier3(target_bytes);
                if (allocated) active_tier_ = AllocationTier::Tier3_Sixteen256MB;
            }
        }
    } else {
        switch (config_.requested_tier) {
            case AllocationTier::Tier1_Single4GB:
                allocated = try_allocate_tier1(target_bytes);
                if (allocated) active_tier_ = AllocationTier::Tier1_Single4GB;
                break;

            case AllocationTier::Tier2_Four1GB:
                allocated = try_allocate_tier2(target_bytes);
                if (allocated) active_tier_ = AllocationTier::Tier2_Four1GB;
                break;

            case AllocationTier::Tier3_Sixteen256MB:
                allocated = try_allocate_tier3(target_bytes);
                if (allocated) active_tier_ = AllocationTier::Tier3_Sixteen256MB;
                break;

            case AllocationTier::Auto:
            default:
                // Auto ladder: Try Tier 1 -> Tier 2 -> Tier 3
                if (try_allocate_tier1(target_bytes)) {
                    active_tier_ = AllocationTier::Tier1_Single4GB;
                    allocated = true;
                } else {
                    if (config_.verbose) std::cout << "[HostSlab] Tier 1 allocation failed/unsupported; attempting Tier 2 (4 x 1 GB)...\n";
                    if (try_allocate_tier2(target_bytes)) {
                        active_tier_ = AllocationTier::Tier2_Four1GB;
                        allocated = true;
                    } else {
                        if (config_.verbose) std::cout << "[HostSlab] Tier 2 allocation failed; attempting Tier 3 (16 x 256 MB)...\n";
                        if (try_allocate_tier3(target_bytes)) {
                            active_tier_ = AllocationTier::Tier3_Sixteen256MB;
                            allocated = true;
                        }
                    }
                }
                break;
        }
    }

    if (!allocated) {
        throw std::runtime_error("[HostSlab] Failed to allocate host pinned slab across all tiers in ladder");
    }

    total_pinned_bytes_ = target_bytes;
    setup_logical_slabs();

    if (config_.verbose) {
        std::cout << "[HostSlab] Initialized " << allocation_tier_to_string(active_tier_)
                  << " | Total Memory: " << (total_pinned_bytes_ / (1024 * 1024)) << " MiB"
                  << " (" << (is_cuda_pinned_ ? "CUDA Pinned" : "Aligned Host Fallback") << ")\n"
                  << "           16 Slabs x 8 Sub-Arenas = 128 Sub-Arenas | Total Slots: "
                  << TOTAL_SLOTS << " (1,280 B each)\n";
    }
}

TieredHostSlabAllocator::~TieredHostSlabAllocator() {
    release_memory();
}

bool TieredHostSlabAllocator::try_allocate_tier1(size_t total_bytes) {
    void* ptr = nullptr;
    bool success = false;

    if (config_.enable_cuda_pinned) {
        cudaError_t err = cudaHostAlloc(&ptr, total_bytes, cudaHostAllocDefault | cudaHostAllocPortable);
        if (err == cudaSuccess && ptr) {
            is_cuda_pinned_ = true;
            success = true;
        }
    }

    if (!success) {
        // Fallback to aligned host memory
#ifdef _WIN32
        ptr = _aligned_malloc(total_bytes, 4096);
#else
        if (posix_memalign(&ptr, 4096, total_bytes) != 0) ptr = nullptr;
#endif
        if (ptr) {
            is_cuda_pinned_ = false;
            success = true;
        }
    }

    if (success) {
        raw_blocks_.push_back(ptr);
        return true;
    }
    return false;
}

bool TieredHostSlabAllocator::try_allocate_tier2(size_t total_bytes) {
    (void)total_bytes;
    constexpr size_t TIER2_SLAB_SIZE = 1ULL * 1024 * 1024 * 1024; // 1 GiB
    constexpr size_t NUM_BLOCKS = 4;
    std::vector<void*> blocks;
    blocks.reserve(NUM_BLOCKS);
    bool use_cuda = config_.enable_cuda_pinned;

    for (size_t i = 0; i < NUM_BLOCKS; ++i) {
        void* ptr = nullptr;
        bool block_ok = false;

        if (use_cuda) {
            cudaError_t err = cudaHostAlloc(&ptr, TIER2_SLAB_SIZE, cudaHostAllocDefault | cudaHostAllocPortable);
            if (err == cudaSuccess && ptr) {
                block_ok = true;
                is_cuda_pinned_ = true;
            }
        }

        if (!block_ok) {
#ifdef _WIN32
            ptr = _aligned_malloc(TIER2_SLAB_SIZE, 4096);
#else
            if (posix_memalign(&ptr, 4096, TIER2_SLAB_SIZE) != 0) ptr = nullptr;
#endif
            if (ptr) {
                block_ok = true;
                is_cuda_pinned_ = false;
            }
        }

        if (!block_ok) {
            // Clean up partially allocated blocks
            for (void* p : blocks) {
                if (is_cuda_pinned_) cudaFreeHost(p);
                else {
#ifdef _WIN32
                    _aligned_free(p);
#else
                    free(p);
#endif
                }
            }
            return false;
        }
        blocks.push_back(ptr);
    }

    raw_blocks_ = std::move(blocks);
    return true;
}

bool TieredHostSlabAllocator::try_allocate_tier3(size_t total_bytes) {
    (void)total_bytes;
    constexpr size_t TIER3_SLAB_SIZE = 256ULL * 1024 * 1024; // 256 MiB
    constexpr size_t NUM_BLOCKS = 16;
    std::vector<void*> blocks;
    blocks.reserve(NUM_BLOCKS);
    bool use_cuda = config_.enable_cuda_pinned;

    for (size_t i = 0; i < NUM_BLOCKS; ++i) {
        void* ptr = nullptr;
        bool block_ok = false;

        if (use_cuda) {
            cudaError_t err = cudaHostAlloc(&ptr, TIER3_SLAB_SIZE, cudaHostAllocDefault | cudaHostAllocPortable);
            if (err == cudaSuccess && ptr) {
                block_ok = true;
                is_cuda_pinned_ = true;
            }
        }

        if (!block_ok) {
#ifdef _WIN32
            ptr = _aligned_malloc(TIER3_SLAB_SIZE, 4096);
#else
            if (posix_memalign(&ptr, 4096, TIER3_SLAB_SIZE) != 0) ptr = nullptr;
#endif
            if (ptr) {
                block_ok = true;
                is_cuda_pinned_ = false;
            }
        }

        if (!block_ok) {
            for (void* p : blocks) {
                if (is_cuda_pinned_) cudaFreeHost(p);
                else {
#ifdef _WIN32
                    _aligned_free(p);
#else
                    free(p);
#endif
                }
            }
            return false;
        }
        blocks.push_back(ptr);
    }

    raw_blocks_ = std::move(blocks);
    return true;
}

void TieredHostSlabAllocator::setup_logical_slabs() {
    switch (active_tier_) {
        case AllocationTier::Tier1_Single4GB: {
            uint8_t* base = static_cast<uint8_t*>(raw_blocks_[0]);
            for (uint32_t i = 0; i < NUM_LOGICAL_SLABS; ++i) {
                logical_slab_ptrs_[i] = base + (static_cast<size_t>(i) * SLAB_SIZE_BYTES);
            }
            break;
        }
        case AllocationTier::Tier2_Four1GB: {
            for (uint32_t i = 0; i < NUM_LOGICAL_SLABS; ++i) {
                size_t raw_idx = i / 4;
                size_t sub_idx = i % 4;
                uint8_t* raw_base = static_cast<uint8_t*>(raw_blocks_[raw_idx]);
                logical_slab_ptrs_[i] = raw_base + (sub_idx * SLAB_SIZE_BYTES);
            }
            break;
        }
        case AllocationTier::Tier3_Sixteen256MB: {
            for (uint32_t i = 0; i < NUM_LOGICAL_SLABS; ++i) {
                logical_slab_ptrs_[i] = static_cast<uint8_t*>(raw_blocks_[i]);
            }
            break;
        }
        default:
            break;
    }

    // Pre-calculate all 128 sub-arena base pointers for sub-nanosecond addressing
    for (uint32_t s = 0; s < TOTAL_SUBARENAS; ++s) {
        uint32_t slab_idx = s / SUBARENAS_PER_SLAB;
        uint32_t sub_in_slab = s % SUBARENAS_PER_SLAB;
        subarena_ptrs_[s] = logical_slab_ptrs_[slab_idx] + (static_cast<size_t>(sub_in_slab) * SUBARENA_STRIDE_BYTES);
    }
}

void TieredHostSlabAllocator::release_memory() {
    for (void* ptr : raw_blocks_) {
        if (!ptr) continue;
        if (is_cuda_pinned_) {
            cudaFreeHost(ptr);
        } else {
#ifdef _WIN32
            _aligned_free(ptr);
#else
            free(ptr);
#endif
        }
    }
    raw_blocks_.clear();
}

uint32_t TieredHostSlabAllocator::allocate_slot(ThreadLocalCache& cache) {
    cache.alloc_requests++;

    // Fast path: L1 thread-local cache hit (0 atomic bus transactions)
    if (cache.count > 0) {
        return cache.cached_slots[--cache.count];
    }

    // Slow path: Batch refill 64 slots from assigned sub-arena
    cache.atomic_bus_transactions++;
    uint32_t batch[ThreadLocalCache::BATCH_SIZE];
    uint32_t allocated = subarenas_[cache.assigned_subarena].allocate_batch(batch, ThreadLocalCache::BATCH_SIZE);

    if (allocated == 0) {
        // Fallback to searching other sub-arenas in round-robin if primary is exhausted
        for (uint32_t s = 1; s < TOTAL_SUBARENAS; ++s) {
            uint32_t next_sub = (cache.assigned_subarena + s) % TOTAL_SUBARENAS;
            allocated = subarenas_[next_sub].allocate_batch(batch, ThreadLocalCache::BATCH_SIZE);
            if (allocated > 0) {
                cache.assigned_subarena = next_sub;
                break;
            }
        }
    }

    if (allocated == 0) {
        return INVALID_SLOT_ID;
    }

    // Populate local cache with remaining slots (1..allocated-1)
    for (size_t i = 1; i < allocated; ++i) {
        cache.cached_slots[cache.count++] = batch[i];
    }

    return batch[0];
}

void TieredHostSlabAllocator::free_slot(uint32_t slot_id, ThreadLocalCache& cache) {
    if (slot_id == INVALID_SLOT_ID || slot_id >= TOTAL_SLOTS) return;
    cache.free_requests++;

    // Fast path: Push to local cache (0 atomic bus transactions)
    if (cache.count < ThreadLocalCache::BATCH_SIZE) {
        cache.cached_slots[cache.count++] = slot_id;
        return;
    }

    // Slow path: Cache is full, drain half (32 slots) back to sub-arena in 1 batch
    cache.atomic_bus_transactions++;
    constexpr size_t DRAIN_COUNT = ThreadLocalCache::BATCH_SIZE / 2;
    uint32_t drain_batch[DRAIN_COUNT];
    for (size_t i = 0; i < DRAIN_COUNT; ++i) {
        drain_batch[i] = cache.cached_slots[--cache.count];
    }

    subarenas_[cache.assigned_subarena].free_batch(drain_batch, DRAIN_COUNT);
    cache.cached_slots[cache.count++] = slot_id;
}

uint32_t TieredHostSlabAllocator::allocate_slot_direct(uint32_t subarena_idx) {
    uint32_t slot = INVALID_SLOT_ID;
    uint32_t target_sub = subarena_idx % TOTAL_SUBARENAS;
    uint32_t count = subarenas_[target_sub].allocate_batch(&slot, 1);
    return (count > 0) ? slot : INVALID_SLOT_ID;
}

void TieredHostSlabAllocator::free_slot_direct(uint32_t slot_id) {
    if (slot_id == INVALID_SLOT_ID || slot_id >= TOTAL_SLOTS) return;

    const uint32_t slab_idx = slot_id / SLOTS_PER_SLAB;
    const uint32_t rem = slot_id % SLOTS_PER_SLAB;
    const uint32_t subarena_in_slab = rem / SLOTS_PER_SUBARENA;
    const uint32_t global_sub = slab_idx * SUBARENAS_PER_SLAB + subarena_in_slab;

    subarenas_[global_sub].free_batch(&slot_id, 1);
}

size_t TieredHostSlabAllocator::active_allocations() const noexcept {
    size_t total = 0;
    for (uint32_t i = 0; i < TOTAL_SUBARENAS; ++i) {
        total += subarenas_[i].active_allocations.load(std::memory_order_relaxed);
    }
    return total;
}

uint64_t TieredHostSlabAllocator::total_atomic_transactions() const noexcept {
    uint64_t total = 0;
    for (uint32_t i = 0; i < TOTAL_SUBARENAS; ++i) {
        total += subarenas_[i].atomic_transactions.load(std::memory_order_relaxed);
    }
    return total;
}

} // namespace hydrogen
