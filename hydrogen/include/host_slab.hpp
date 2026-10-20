#pragma once

#include <cstdint>
#include <cstddef>
#include <atomic>
#include <vector>
#include <array>
#include <memory>
#include <mutex>
#include <string>

namespace hydrogen {

// ============================================================================
// Host Pinned Slab Geometry & Sizing Constants
// ============================================================================

/// @brief Slot payload size matching uniform page block KV cache payload (1,280 bytes).
inline constexpr size_t SLOT_SIZE_BYTES = 1280;

/// @brief Number of slots in each sub-arena (26,214 slots * 1,280 B = 33,553,920 B ~ 32 MiB).
inline constexpr uint32_t SLOTS_PER_SUBARENA = 26214;

/// @brief Number of independent sub-arenas per 256 MiB slab.
inline constexpr uint32_t SUBARENAS_PER_SLAB = 8;

/// @brief Total slots per 256 MiB slab (8 * 26,214 = 209,712 slots).
inline constexpr uint32_t SLOTS_PER_SLAB = SUBARENAS_PER_SLAB * SLOTS_PER_SUBARENA; // 209,712

/// @brief Stride in bytes for each sub-arena inside a slab (32 MiB = 33,554,432 bytes).
inline constexpr size_t SUBARENA_STRIDE_BYTES = 32ULL * 1024 * 1024; // 33,554,432 B

/// @brief Logical slab size in bytes (256 MiB = 268,435,456 bytes).
inline constexpr size_t SLAB_SIZE_BYTES = 256ULL * 1024 * 1024; // 268,435,456 B

/// @brief Total number of 256 MiB logical slabs composing the 4 GiB pool.
inline constexpr uint32_t NUM_LOGICAL_SLABS = 16;

/// @brief Total sub-arenas across the entire 4 GiB allocator (16 slabs * 8 sub-arenas = 128).
inline constexpr uint32_t TOTAL_SUBARENAS = NUM_LOGICAL_SLABS * SUBARENAS_PER_SLAB; // 128

/// @brief Total slot capacity across the full 4 GiB pool (16 * 209,712 = 3,355,392 slots).
inline constexpr uint32_t TOTAL_SLOTS = NUM_LOGICAL_SLABS * SLOTS_PER_SLAB; // 3,355,392

/// @brief Strict maximum pinned memory cap (4 GiB = 4,294,967,296 bytes).
inline constexpr size_t MAX_PINNED_BYTES = 4ULL * 1024 * 1024 * 1024; // 4,294,967,296 B

/// @brief Sentinel for invalid or failed slot allocations.
inline constexpr uint32_t INVALID_SLOT_ID = 0xFFFFFFFF;

// ============================================================================
// Allocation Tier Ladder
// ============================================================================

enum class AllocationTier : uint8_t {
    Auto = 0,               ///< Attempt Tier 1, fallback to Tier 2, then Tier 3
    Tier1_Single4GB = 1,    ///< 1 contiguous 4 GiB pinned block
    Tier2_Four1GB = 2,      ///< Array of four 1 GiB pinned slabs
    Tier3_Sixteen256MB = 3, ///< Array of sixteen 256 MiB pinned slabs
};

inline const char* allocation_tier_to_string(AllocationTier tier) noexcept {
    switch (tier) {
        case AllocationTier::Auto: return "Auto";
        case AllocationTier::Tier1_Single4GB: return "Tier 1 (1 x 4 GB)";
        case AllocationTier::Tier2_Four1GB: return "Tier 2 (4 x 1 GB)";
        case AllocationTier::Tier3_Sixteen256MB: return "Tier 3 (16 x 256 MB)";
        default: return "Unknown";
    }
}

// ============================================================================
// Configuration
// ============================================================================

struct HostSlabConfig {
    AllocationTier requested_tier = AllocationTier::Auto;
    bool simulate_fragmentation = false; ///< Simulate kernel fragmentation to force Tier 2/3
    bool force_tier3_fragmentation = false; ///< Force fallback all the way to Tier 3
    bool enable_cuda_pinned = true;      ///< Allocate via cudaHostAlloc; fallback to aligned host
    bool verbose = true;                 ///< Print initialization info to stdout
    size_t custom_max_pinned_bytes = MAX_PINNED_BYTES;
};

// ============================================================================
// Cache-Line Aligned Sub-Arena
// ============================================================================

/**
 * @brief Independent sub-arena aligned to 64-byte hardware cache lines.
 *
 * Each sub-arena owns its own bump index, free-stack, and synchronization primitives,
 * preventing L3 cache line false sharing (cache bouncing) across concurrent worker threads.
 */
struct alignas(64) SubArena {
    // Cache Line 1 (0..63 bytes): Hot allocation bump index & counter
    std::atomic<uint32_t> bump_index{0};
    std::atomic<uint32_t> active_allocations{0};
    std::atomic<uint64_t> atomic_transactions{0};
    uint32_t global_subarena_id{0};
    uint32_t max_slots{SLOTS_PER_SUBARENA};
    uint8_t pad1[64 - sizeof(std::atomic<uint32_t>) * 2 - sizeof(std::atomic<uint64_t>) - sizeof(uint32_t) * 2];

    // Cache Line 2 (64..127 bytes): Free-list synchronization
    mutable std::mutex free_lock;
    std::vector<uint32_t> free_stack;
    uint8_t pad2[64 - (sizeof(std::mutex) + sizeof(std::vector<uint32_t>)) % 64];

    SubArena();
    ~SubArena() = default;

    // Non-copyable, non-movable
    SubArena(const SubArena&) = delete;
    SubArena& operator=(const SubArena&) = delete;
    SubArena(SubArena&&) = delete;
    SubArena& operator=(SubArena&&) = delete;

    /// @brief Allocate up to `batch_size` slots from this sub-arena.
    uint32_t allocate_batch(uint32_t* out_slots, uint32_t batch_size);

    /// @brief Return up to `batch_size` recycled slots to this sub-arena.
    void free_batch(const uint32_t* in_slots, uint32_t batch_size);
};

// ============================================================================
// Thread-Local 64-Slot Free-List Cache
// ============================================================================

/**
 * @brief Worker thread-local cache holding up to 64 slots.
 *
 * Eliminates 98.4% of atomic bus transactions by serving allocations and frees
 * directly from local L1/register memory. Refills and flushes in batches of 64.
 */
struct ThreadLocalCache {
    static constexpr size_t BATCH_SIZE = 64;
    std::array<uint32_t, BATCH_SIZE> cached_slots{};
    size_t count = 0;
    uint32_t assigned_subarena = 0;

    // Local telemetry counters
    uint64_t alloc_requests = 0;
    uint64_t free_requests = 0;
    uint64_t atomic_bus_transactions = 0;

    explicit ThreadLocalCache(uint32_t subarena_id = 0) noexcept
        : assigned_subarena(subarena_id % TOTAL_SUBARENAS) {}
};

// ============================================================================
// Tiered Host Pinned Slab Allocator
// ============================================================================

class TieredHostSlabAllocator {
public:
    explicit TieredHostSlabAllocator(HostSlabConfig config = {});
    ~TieredHostSlabAllocator();

    // Non-copyable, non-movable
    TieredHostSlabAllocator(const TieredHostSlabAllocator&) = delete;
    TieredHostSlabAllocator& operator=(const TieredHostSlabAllocator&) = delete;
    TieredHostSlabAllocator(TieredHostSlabAllocator&&) = delete;
    TieredHostSlabAllocator& operator=(TieredHostSlabAllocator&&) = delete;

    /**
     * @brief Allocate a slot using the worker thread's local cache.
     * @param cache Thread-local 64-slot free-list cache.
     * @return Global slot ID, or INVALID_SLOT_ID if exhausted.
     */
    [[nodiscard]] uint32_t allocate_slot(ThreadLocalCache& cache);

    /**
     * @brief Free a slot back to the worker thread's local cache.
     * @param slot_id Global slot ID to release.
     * @param cache Thread-local 64-slot free-list cache.
     */
    void free_slot(uint32_t slot_id, ThreadLocalCache& cache);

    /**
     * @brief Direct allocation bypassing thread-local cache (for testing or baseline).
     * @param subarena_idx Specific sub-arena index (0..127).
     * @return Global slot ID, or INVALID_SLOT_ID if full.
     */
    [[nodiscard]] uint32_t allocate_slot_direct(uint32_t subarena_idx);

    /**
     * @brief Direct free bypassing thread-local cache.
     * @param slot_id Global slot ID to release.
     */
    void free_slot_direct(uint32_t slot_id);

    /**
     * @brief Translate global slot ID to physical host memory pointer.
     *
     * Computes slab index and sub-arena offset via single-cycle reciprocal multiplication.
     * Execution latency is strictly < 1.0 ns/op.
     *
     * @param slot_id Global slot index (0..3,355,391).
     * @return Pointer to 1,280-byte slot in pinned host RAM.
     */
    [[nodiscard]] inline void* get_slot_address(uint32_t slot_id) const noexcept {
        const uint32_t global_sub = slot_id / SLOTS_PER_SUBARENA;
        const uint32_t slot_offset = slot_id % SLOTS_PER_SUBARENA;
        return subarena_ptrs_[global_sub] + (static_cast<size_t>(slot_offset) * SLOT_SIZE_BYTES);
    }

    // --- State & Telemetry Accessors ---
    [[nodiscard]] AllocationTier active_tier() const noexcept { return active_tier_; }
    [[nodiscard]] bool is_cuda_pinned() const noexcept { return is_cuda_pinned_; }
    [[nodiscard]] size_t total_pinned_bytes() const noexcept { return total_pinned_bytes_; }
    [[nodiscard]] size_t total_slots() const noexcept { return TOTAL_SLOTS; }
    [[nodiscard]] size_t active_allocations() const noexcept;
    [[nodiscard]] uint64_t total_atomic_transactions() const noexcept;
    [[nodiscard]] const SubArena& get_subarena(uint32_t index) const { return subarenas_[index % TOTAL_SUBARENAS]; }

private:
    HostSlabConfig config_;
    AllocationTier active_tier_ = AllocationTier::Tier1_Single4GB;
    bool is_cuda_pinned_ = false;
    size_t total_pinned_bytes_ = 0;

    // Pointers to physically allocated memory blocks
    std::vector<void*> raw_blocks_;

    // Uniform 16 logical slab pointers (each 256 MiB)
    std::array<uint8_t*, NUM_LOGICAL_SLABS> logical_slab_ptrs_{};

    // Fast-lookup 128 sub-arena base pointers (each 32 MiB)
    std::array<uint8_t*, TOTAL_SUBARENAS> subarena_ptrs_{};

    // 128 cache-line aligned sub-arenas
    std::unique_ptr<SubArena[]> subarenas_;

    bool try_allocate_tier1(size_t total_bytes);
    bool try_allocate_tier2(size_t total_bytes);
    bool try_allocate_tier3(size_t total_bytes);
    void setup_logical_slabs();
    void release_memory();
};

} // namespace hydrogen
