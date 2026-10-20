#include "dma_guard.hpp"

#include <iostream>
#include <iomanip>
#include <chrono>
#include <thread>

namespace hydrogen {

WddmTdrGuard::WddmTdrGuard(uint32_t timeout_ms)
    : timeout_ms_(timeout_ms) {}

bool WddmTdrGuard::wait_quiescence_custom(const std::function<cudaError_t()>& query_fn,
                                          const std::function<void()>& on_timeout_cb) {
    reset_state();
    const auto start = std::chrono::steady_clock::now();

    while (true) {
        cudaError_t err = cudaSuccess;
        if (query_fn) {
            err = query_fn();
        }

        if (err == cudaSuccess) {
            const auto elapsed = std::chrono::duration_cast<std::chrono::microseconds>(
                std::chrono::steady_clock::now() - start).count();
            last_poll_duration_ms_ = static_cast<double>(elapsed) / 1000.0;
            return true;
        }

        // Handle environments without CUDA devices (e.g. headless CI fallback)
        if (err != cudaErrorNotReady) {
            if (err == cudaErrorNoDevice || err == cudaErrorInsufficientDriver) {
                last_poll_duration_ms_ = 0.0;
                return true;
            }
        }

        const auto elapsed_ms = std::chrono::duration_cast<std::chrono::milliseconds>(
            std::chrono::steady_clock::now() - start).count();

        if (elapsed_ms >= timeout_ms_) {
            // 500 ms backstop exceeded: 4x safety factor before Windows 2,000 ms TdrDelay
            last_poll_duration_ms_ = static_cast<double>(elapsed_ms);
            restart_triggered_ = true;

            std::cerr << "[HYDROGEN TDR GUARD] PCIe DMA transfer hung (>" << elapsed_ms
                      << " ms, ceiling=" << timeout_ms_
                      << " ms); executing controlled engine restart to prevent Windows WDDM TDR reset (0x117 / nvlddmkm.sys)\n";

            if (on_timeout_cb) {
                on_timeout_cb();
            }
            return false;
        }

        std::this_thread::sleep_for(std::chrono::microseconds(WDDM_TDR_POLL_INTERVAL_US));
    }
}

bool WddmTdrGuard::wait_quiescence(cudaStream_t stream,
                                   const std::function<void()>& on_timeout_cb) {
    return wait_quiescence_custom([stream]() -> cudaError_t {
        return cudaStreamQuery(stream);
    }, on_timeout_cb);
}

bool WddmTdrGuard::wait_quiescence_simulated(uint32_t simulated_stall_ms,
                                             const std::function<void()>& on_timeout_cb) {
    const auto sim_start = std::chrono::steady_clock::now();
    return wait_quiescence_custom([sim_start, simulated_stall_ms]() -> cudaError_t {
        const auto now = std::chrono::steady_clock::now();
        const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(now - sim_start).count();
        if (elapsed >= simulated_stall_ms) {
            return cudaSuccess;
        }
        return cudaErrorNotReady;
    }, on_timeout_cb);
}

} // namespace hydrogen
