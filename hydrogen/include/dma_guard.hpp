#pragma once

#include <cstdint>
#include <chrono>
#include <functional>
#include <atomic>
#include <thread>
#include <iostream>

#include <cuda_runtime.h>

namespace hydrogen {

// ============================================================================
// WDDM TDR Timing Constants
// ============================================================================

/// @brief 500 ms non-blocking WDDM TDR backstop (4x safety factor before Windows 2,000 ms TdrDelay).
inline constexpr uint32_t WDDM_TDR_BACKSTOP_MS = 500;

/// @brief Non-blocking stream polling interval in microseconds (100 us).
inline constexpr uint32_t WDDM_TDR_POLL_INTERVAL_US = 100;

/// @brief Default Windows WDDM TdrDelay in milliseconds (2,000 ms).
inline constexpr uint32_t WINDOWS_WDDM_TDR_DELAY_MS = 2000;

// ============================================================================
// WDDM TDR 500 ms Guard
// ============================================================================

class WddmTdrGuard {
public:
    explicit WddmTdrGuard(uint32_t timeout_ms = WDDM_TDR_BACKSTOP_MS);
    ~WddmTdrGuard() = default;

    /**
     * @brief Polls stream completion via non-blocking cudaStreamQuery bounded by timeout_ms.
     * Prevents Windows WDDM 2,000 ms TdrDelay display driver reset.
     * @param stream The CUDA stream executing DMA copy operations.
     * @param on_timeout_cb Callback invoked if transfer hangs > timeout_ms (triggers safe restart).
     * @return True if stream reached quiescence cleanly; False if transfer timed out (>500 ms).
     */
    bool wait_quiescence(cudaStream_t stream, const std::function<void()>& on_timeout_cb = nullptr);

    /**
     * @brief Generic predicate polling loop with 500 ms ceiling and 100 us intervals.
     * Useful for testing, simulation, and custom asynchronous query probes.
     * @param query_fn Predicate returning cudaSuccess when finished, cudaErrorNotReady while pending.
     * @param on_timeout_cb Callback invoked if query hangs > timeout_ms.
     * @return True if query reached quiescence cleanly; False if timed out (>500 ms).
     */
    bool wait_quiescence_custom(const std::function<cudaError_t()>& query_fn,
                                const std::function<void()>& on_timeout_cb = nullptr);

    /**
     * @brief Simulates a DMA transfer stall duration and verifies backstop enforcement.
     * @param simulated_stall_ms Duration the simulated hardware transfer remains unready.
     * @param on_timeout_cb Callback invoked if simulated stall exceeds timeout_ms.
     * @return True if transfer completed within backstop; False if backstop tripped.
     */
    bool wait_quiescence_simulated(uint32_t simulated_stall_ms,
                                   const std::function<void()>& on_timeout_cb = nullptr);

    [[nodiscard]] bool restart_triggered() const noexcept { return restart_triggered_; }
    [[nodiscard]] double last_poll_duration_ms() const noexcept { return last_poll_duration_ms_; }
    [[nodiscard]] uint32_t timeout_ms() const noexcept { return timeout_ms_; }
    [[nodiscard]] double safety_margin_factor() const noexcept {
        return static_cast<double>(WINDOWS_WDDM_TDR_DELAY_MS) / static_cast<double>(timeout_ms_);
    }

    void reset_state() noexcept {
        restart_triggered_ = false;
        last_poll_duration_ms_ = 0.0;
    }

private:
    uint32_t timeout_ms_;
    bool restart_triggered_ = false;
    double last_poll_duration_ms_ = 0.0;
};

} // namespace hydrogen
