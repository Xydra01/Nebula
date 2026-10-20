#pragma once

#include <cstdint>
#include <cstddef>
#include <atomic>
#include <vector>
#include <string>
#include <thread>
#include <chrono>

// CUDA Runtime forward declarations and types
#include <cuda_runtime.h>

namespace hydrogen {

// ============================================================================
// DMA Swapper Geometry & Timing Constants
// ============================================================================

/// @brief Total leaf KV cache size to swap (160 MiB = 167,772,160 bytes).
inline constexpr size_t SWAP_LEAF_BYTES = 160ULL * 1024 * 1024; // 167,772,160 B

/// @brief Fine-grained transfer chunk for mid-flight cancellation (2 MiB = 2,097,152 bytes).
inline constexpr size_t SWAP_BATCH_BYTES = 2ULL * 1024 * 1024; // 2,097,152 B

/// @brief Total number of fine-grained transfer chunks (160 MiB / 2 MiB = 80 batches).
inline constexpr size_t SWAP_NUM_BATCHES = SWAP_LEAF_BYTES / SWAP_BATCH_BYTES; // 80

/// @brief Maximum allowed latency ceiling for 160 MB transfer over PCIe 4.0 x16.
inline constexpr double MAX_SWAP_LATENCY_CEILING_MS = 7.5;
inline constexpr double MAX_SWAP_LATENCY_CEILING_PCIE4_MS = 7.5;

/// @brief Maximum allowed latency ceiling for 160 MB transfer over PCIe 3.0 x16.
inline constexpr double MAX_SWAP_LATENCY_CEILING_PCIE3_MS = 15.0;

/// @brief Maximum allowed recovery latency from mid-flight cancellation to RUNNABLE.
inline constexpr double MAX_CANCELLATION_RECOVERY_CEILING_MS = 1.0;

// ============================================================================
// Three-Phase State Machine
// ============================================================================

enum class SwapState : uint8_t {
    RUNNABLE  = 0, ///< Cache resident in VRAM; active inference decoder runnable
    EVICTING  = 1, ///< Transferring pages asynchronously from VRAM to host pinned RAM
    SWAPPED   = 2, ///< Cache completely evicted; VRAM reclaimed; pages in host pinned RAM
    RESTORING = 3  ///< Transferring pages asynchronously from host pinned RAM back to VRAM
};

inline const char* swap_state_to_string(SwapState state) noexcept {
    switch (state) {
        case SwapState::RUNNABLE:  return "RUNNABLE";
        case SwapState::EVICTING:  return "EVICTING";
        case SwapState::SWAPPED:   return "SWAPPED";
        case SwapState::RESTORING: return "RESTORING";
        default:                   return "UNKNOWN";
    }
}

// ============================================================================
// Configuration & Telemetry
// ============================================================================

struct DmaSwapperConfig {
    size_t total_bytes = SWAP_LEAF_BYTES;
    size_t batch_size = SWAP_BATCH_BYTES;
    int stream_priority = -1;           ///< Highest priority CUDA stream (-1)
    bool force_cpu_emulation = false;   ///< Force CPU fallback for headless CI testing
    bool verbose = true;                ///< Print diagnostic log statements
};

struct SwapTelemetry {
    double eviction_duration_ms = 0.0;
    double restoration_duration_ms = 0.0;
    double pcie4_normalized_eviction_ms = 0.0;
    double pcie4_normalized_restoration_ms = 0.0;
    double cancellation_recovery_ms = 0.0;
    size_t bytes_transferred = 0;
    size_t batches_evicted = 0;
    size_t batches_restored = 0;
    int pcie_link_gen = 4;
    bool cancelled_early = false;
    bool integrity_verified = false;
};

// ============================================================================
// Asynchronous PCIe 4.0 DMA Swap Stream & Cancellation Barrier
// ============================================================================

class DmaSwapper {
public:
    explicit DmaSwapper(DmaSwapperConfig config = {});
    ~DmaSwapper();

    // Non-copyable, non-movable
    DmaSwapper(const DmaSwapper&) = delete;
    DmaSwapper& operator=(const DmaSwapper&) = delete;
    DmaSwapper(DmaSwapper&&) = delete;
    DmaSwapper& operator=(DmaSwapper&&) = delete;

    // --- State Machine Accessors ---
    [[nodiscard]] SwapState state() const noexcept {
        return state_.load(std::memory_order_acquire);
    }

    [[nodiscard]] bool is_cancelled() const noexcept {
        return cancel_eviction_.load(std::memory_order_acquire);
    }

    [[nodiscard]] bool is_cuda_active() const noexcept {
        return is_cuda_active_;
    }

    [[nodiscard]] int pcie_link_gen() const noexcept {
        return pcie_link_gen_;
    }

    [[nodiscard]] size_t total_bytes() const noexcept {
        return config_.total_bytes;
    }

    // --- Asynchronous Execution Interface ---

    /**
     * @brief Asynchronously evict 160 MB leaf KV cache from VRAM to host pinned RAM.
     * Transitions state: RUNNABLE -> EVICTING -> SWAPPED.
     */
    void evict_async();

    /**
     * @brief Asynchronously restore 160 MB leaf KV cache from host pinned RAM to VRAM.
     * Transitions state: SWAPPED -> RESTORING -> RUNNABLE.
     */
    void restore_async();

    /**
     * @brief Signal cancellation barrier during eviction (early tool completion).
     * If currently in EVICTING, aborts remaining batches, reverse-restores only
     * transferred host pages, and transitions immediately back to RUNNABLE in < 1.0 ms.
     */
    void request_cancel();

    /**
     * @brief Synchronize the copy stream and wait until current swap state stabilizes.
     */
    void wait_idle();

    // --- Test & Proof Verification Helpers ---

    /**
     * @brief Execute full 160 MB bidirectional evacuation and restoration.
     * @return Telemetry with measured eviction and restoration latencies.
     */
    SwapTelemetry run_full_swap();

    /**
     * @brief Execute cancellation barrier test: signals tool completion `cancel_after_ms` into eviction.
     * @param cancel_after_ms Milliseconds to wait before asserting cancellation barrier.
     * @return Telemetry measuring cancellation recovery time and integrity.
     */
    SwapTelemetry run_cancellation_test(double cancel_after_ms = 2.0);

    /**
     * @brief Benchmark concurrent compute stream during swap to verify zero engine stalls.
     * @return True if compute stream runs concurrently without stalls.
     */
    bool benchmark_concurrent_compute();

    /**
     * @brief Populate VRAM with deterministic hash test pattern.
     */
    void populate_test_pattern();

    /**
     * @brief Verify that VRAM payload matches the golden test pattern checksum.
     */
    [[nodiscard]] bool verify_data_integrity();

private:
    DmaSwapperConfig config_;
    std::atomic<SwapState> state_{SwapState::RUNNABLE};
    std::atomic<bool> cancel_eviction_{false};
    bool is_cuda_active_ = false;

    // CUDA stream and memory buffers
    cudaStream_t swap_stream_ = nullptr;
    std::vector<cudaEvent_t> batch_events_;
    void* d_vram_ = nullptr;     ///< Device VRAM buffer (160 MiB)
    void* h_pinned_ = nullptr;   ///< Pinned Host RAM buffer (160 MiB)

    // CPU fallback buffers if no CUDA hardware is present
    std::vector<uint8_t> emu_vram_;
    std::vector<uint8_t> emu_host_;

    // Tracking for atomic cancellation recovery
    std::atomic<size_t> completed_eviction_batches_{0};
    uint64_t golden_checksum_ = 0;

    void allocate_buffers();
    void free_buffers();
    void detect_pcie_topology();
    uint64_t compute_checksum(const void* buffer, size_t size);

    int pcie_link_gen_ = 4;
};

} // namespace hydrogen
