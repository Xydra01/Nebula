#pragma once

#include <cstdint>
#include <cstddef>
#include <atomic>
#include <vector>
#include <deque>
#include <chrono>
#include <memory>
#include <mutex>
#include <string>
#include <functional>
#include <thread>
#include <iostream>

#include <cuda_runtime.h>
#include "dma_guard.hpp"

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#endif

namespace hydrogen {

// ============================================================================
// Watermark & WDDM Timing Constants
// ============================================================================

/// @brief Total host-pinned KV swap memory capacity (4 GiB = 4,294,967,296 bytes).
inline constexpr size_t POOL_TOTAL_CAPACITY_BYTES = 4096ULL * 1024 * 1024;

/// @brief 3.5 GiB High-Water Mark (3,584 MiB = 3,758,096,384 bytes = 87.5% of pool).
inline constexpr size_t SWAP_HIGH_WATER_MARK_BYTES = 3584ULL * 1024 * 1024;

/// @brief Leaf KV cache swap unit (160 MiB = 167,772,160 bytes).
inline constexpr size_t LEAF_KV_CACHE_BYTES = 160ULL * 1024 * 1024;

/// @brief 30-second hard watermark auto-reclaim timeout for paused streams.
inline constexpr uint32_t WATERMARK_CLEAR_TIMEOUT_MS = 30000;

// ============================================================================
// Stream Lifecycle State Machine
// ============================================================================

enum class StreamState : uint8_t {
    RUNNABLE = 0,             ///< Resident in VRAM; active token decoding
    AWAITING_TOOL = 1,        ///< Tool launched in child Job Object; eligible for swap
    EVICTING = 2,             ///< Asynchronously copying pages from VRAM to host pinned RAM
    SWAPPED = 3,              ///< Resident in host pinned RAM; VRAM reclaimed
    VRAM_RESIDENT_PAUSED = 4, ///< Held resident in VRAM because swapped pool is >= 3.5 GB
    RESTORING = 5,            ///< Asynchronously copying pages from host pinned RAM back to VRAM
    TERMINATED_ZOMBIE = 6     ///< Terminated via TerminateJobObject after 30s watermark timeout
};

inline const char* stream_state_to_string(StreamState s) noexcept {
    switch (s) {
        case StreamState::RUNNABLE:             return "RUNNABLE";
        case StreamState::AWAITING_TOOL:        return "AWAITING_TOOL";
        case StreamState::EVICTING:             return "EVICTING";
        case StreamState::SWAPPED:              return "SWAPPED";
        case StreamState::VRAM_RESIDENT_PAUSED: return "VRAM_RESIDENT_PAUSED";
        case StreamState::RESTORING:            return "RESTORING";
        case StreamState::TERMINATED_ZOMBIE:    return "TERMINATED_ZOMBIE";
        default:                                return "UNKNOWN";
    }
}

// ============================================================================
// Tool Process & Job Object Descriptor
// ============================================================================

struct ToolProcessDescriptor {
    uint32_t stream_id = 0;
    std::string tool_name;
    size_t memory_bytes = LEAF_KV_CACHE_BYTES;
    std::chrono::steady_clock::time_point launch_time;
    std::chrono::steady_clock::time_point paused_time;
    StreamState state = StreamState::RUNNABLE;

#ifdef _WIN32
    HANDLE h_job = nullptr;
    HANDLE h_process = nullptr;
    DWORD pid = 0;
#else
    void* h_job = nullptr;
    void* h_process = nullptr;
    uint32_t pid = 0;
#endif

    bool is_hung = false;          ///< Simulated hang condition (infinite loop / deadlocked socket)
    bool was_terminated = false;   ///< Flag set when TerminateJobObject was called
};

// ============================================================================
// Watermark Supervisor Telemetry
// ============================================================================

struct SupervisorTelemetry {
    size_t active_swapped_bytes = 0;
    size_t high_water_mark_bytes = SWAP_HIGH_WATER_MARK_BYTES;
    size_t total_capacity_bytes = POOL_TOTAL_CAPACITY_BYTES;
    size_t total_registered_streams = 0;
    size_t streams_swapped = 0;
    size_t streams_paused = 0;
    size_t zombie_processes_reclaimed = 0;
    size_t slots_reclaimed_bytes = 0;
    double max_tdr_poll_duration_ms = 0.0;
    bool wddm_tdr_tripped = false;
    bool quiescence_enforced = false;
    bool deadlocks_detected = false;
};

// ============================================================================
// Watermark Supervisor
// ============================================================================

class WatermarkSupervisor {
public:
    WatermarkSupervisor(size_t max_capacity = POOL_TOTAL_CAPACITY_BYTES,
                        size_t high_water_mark = SWAP_HIGH_WATER_MARK_BYTES,
                        uint32_t watermark_timeout_ms = WATERMARK_CLEAR_TIMEOUT_MS,
                        uint32_t tdr_backstop_ms = WDDM_TDR_BACKSTOP_MS);
    ~WatermarkSupervisor();

    // Non-copyable, non-movable
    WatermarkSupervisor(const WatermarkSupervisor&) = delete;
    WatermarkSupervisor& operator=(const WatermarkSupervisor&) = delete;
    WatermarkSupervisor(WatermarkSupervisor&&) = delete;
    WatermarkSupervisor& operator=(WatermarkSupervisor&&) = delete;

    // Stream lifecycle
    uint32_t register_stream(const std::string& tool_name, bool is_hung = false);
    bool transition_awaiting_tool(uint32_t stream_id);
    bool complete_tool(uint32_t stream_id);

    // Timeout evaluation & auto-reclaim
    size_t check_and_reclaim_zombies(std::chrono::milliseconds simulated_elapsed = std::chrono::milliseconds(0));

    // Accessors
    [[nodiscard]] size_t current_swapped_bytes() const;
    [[nodiscard]] size_t paused_stream_count() const;
    [[nodiscard]] size_t swapped_stream_count() const;
    [[nodiscard]] StreamState get_stream_state(uint32_t stream_id) const;
    [[nodiscard]] SupervisorTelemetry get_telemetry() const;

    // TDR guard and DMA quiescence
    bool ensure_dma_quiescence(cudaStream_t stream);
    bool test_simulated_dma_stall(uint32_t stall_duration_ms);

    void set_watermark_timeout_ms(uint32_t timeout_ms) {
        watermark_timeout_ms_ = timeout_ms;
    }

private:
    size_t max_capacity_;
    size_t high_water_mark_;
    uint32_t watermark_timeout_ms_;

    mutable std::mutex mutex_;
    std::vector<ToolProcessDescriptor> streams_;
    std::deque<uint32_t> paused_queue_; // Stream IDs currently in VRAM_RESIDENT_PAUSED

    size_t active_swapped_bytes_ = 0;
    size_t zombie_reclaimed_count_ = 0;
    size_t slots_reclaimed_bytes_ = 0;
    bool quiescence_enforced_ = false;

    WddmTdrGuard tdr_guard_;
    bool engine_restarting_ = false;

    void terminate_job_process(ToolProcessDescriptor& desc);
    void unfreeze_paused_streams();
};

} // namespace hydrogen
