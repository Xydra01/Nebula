// hydrogen/include/kernel_tiler.hpp
// WDDM sub-5ms kernel execution clamping and micro-chunked tiler (Issue H09)

#pragma once

#include <cstdint>
#include <cstddef>
#include <vector>
#include <span>
#include "page_block.hpp"

namespace hydrogen {

/**
 * @brief Configuration parameters for WDDM sub-5ms kernel execution clamping.
 */
struct WddmClampingConfig {
    float max_quantum_ms = 5.0f;       // WDDM sub-5ms clamping threshold
    uint32_t head_dim = 128;           // Target 9B geometry (d_head = 128)
    uint32_t num_q_heads = 32;         // 32 Q heads
    uint32_t num_kv_heads = 8;         // 8 KV heads (GQA 4:1)
    uint32_t max_seq_chunk = 1024;     // Max token chunk per kernel launch
    uint32_t max_batch_size = 16;      // Max concurrent streams
};

/**
 * @brief Micro-chunked tile descriptor for safe WDDM yielding.
 */
struct WddmKernelTile {
    uint32_t tile_id = 0;
    uint32_t start_token = 0;
    uint32_t num_tokens = 0;
    float estimated_ms = 0.0f;
};

/**
 * @brief Benchmark results across continuous execution steps under WDDM clamping.
 */
struct WddmTilerBenchmarkResult {
    uint32_t total_steps = 10000;
    uint32_t total_tiles_launched = 0;
    float max_kernel_duration_ms = 0.0f;
    float avg_kernel_duration_ms = 0.0f;
    float min_kernel_duration_ms = 0.0f;
    uint32_t preemption_stalls = 0;
    bool clamping_invariant_held = true; // max_kernel_duration_ms < 5.0 ms
    double divergent_warp_efficiency = 0.0;
    double templated_warp_efficiency = 0.0;
    double speedup_ratio = 1.0;
};

/**
 * @brief WDDM Kernel Tiler and Execution Engine on SM 8.9.
 */
class WddmKernelTiler {
public:
    explicit WddmKernelTiler(WddmClampingConfig config);
    ~WddmKernelTiler();

    // Non-copyable, non-movable
    WddmKernelTiler(const WddmKernelTiler&) = delete;
    WddmKernelTiler& operator=(const WddmKernelTiler&) = delete;
    WddmKernelTiler(WddmKernelTiler&&) = delete;
    WddmKernelTiler& operator=(WddmKernelTiler&&) = delete;

    /**
     * @brief Partition sequence or batch tokens into micro-chunked tiles strictly < 5.0 ms.
     */
    [[nodiscard]] std::vector<WddmKernelTile> tile_workload(uint32_t total_tokens) const;

    /**
     * @brief Run divergent baseline attention over mixed pages (exhibiting warp divergence).
     */
    void launch_divergent_attention(
        const float* d_q,
        const uint8_t* d_k_pages,
        const uint8_t* d_v_pages,
        const uint8_t* d_quant_types,
        const float* d_scales,
        float* d_out,
        uint32_t start_token,
        uint32_t num_tokens,
        uint32_t batch_size
    );

    /**
     * @brief Run specialized templated attention with zero warp divergence.
     */
    template <PageQuantType Q>
    void launch_templated_attention(
        const float* d_q,
        const uint8_t* d_k_pages,
        const uint8_t* d_v_pages,
        const float* d_scales,
        float* d_out,
        uint32_t start_token,
        uint32_t num_tokens,
        uint32_t batch_size
    );

    /**
     * @brief Dispatch clamped micro-chunked tiles ensuring all kernel durations strictly < 5.0 ms.
     * @return Max kernel duration observed in ms.
     */
    float execute_clamped_workload(uint32_t total_tokens, uint32_t batch_size, bool use_templated);

    /**
     * @brief Run comprehensive benchmark sweep across up to 10,000 steps and deep sequences.
     */
    WddmTilerBenchmarkResult run_clamping_sweep(uint32_t steps = 1000);

    [[nodiscard]] const WddmClampingConfig& config() const noexcept { return config_; }

private:
    WddmClampingConfig config_;

    // Device memory buffers
    float* d_q_{nullptr};
    uint8_t* d_k_pages_{nullptr};
    uint8_t* d_v_pages_{nullptr};
    uint8_t* d_quant_types_{nullptr};
    float* d_scales_{nullptr};
    float* d_out_{nullptr};

    void allocate_device_memory(size_t max_tokens, size_t max_batch);
    void free_device_memory();
    void initialize_synthetic_data(size_t max_tokens, size_t max_batch);
};

} // namespace hydrogen
