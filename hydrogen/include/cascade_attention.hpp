#pragma once

#include <cstdint>
#include <cstddef>
#include <vector>
#include <span>

namespace hydrogen {

/**
 * @brief Configuration parameters for Cascade Attention evaluation.
 */
struct CascadeAttentionConfig {
    uint32_t batch_size = 8;            // Number of concurrent streams (e.g. 1, 2, 4, 8)
    uint32_t head_dim = 128;            // Dimension per head (D)
    uint32_t num_q_heads = 32;          // Number of query heads
    uint32_t num_kv_heads = 8;          // Number of KV heads (GQA 4:1)
    uint32_t prefix_len = 4096;         // Shared prefix token length
    uint32_t leaf_len = 128;            // Private leaf token length per stream
};

/**
 * @brief Execution metrics and numerical fidelity results.
 */
struct CascadeBenchmarkResult {
    uint32_t batch_size = 1;
    float latency_ms = 0.0f;
    double throughput_tok_s = 0.0;
    double speedup_ratio = 1.0;
    double max_abs_error = 0.0;
    double mean_squared_error = 0.0;
    bool numerical_match = true;
};

/**
 * @brief FlashInfer Cascade Attention SM 8.9 Engine.
 */
class CascadeAttentionEngine {
public:
    explicit CascadeAttentionEngine(CascadeAttentionConfig config);
    ~CascadeAttentionEngine();

    // Non-copyable, non-movable
    CascadeAttentionEngine(const CascadeAttentionEngine&) = delete;
    CascadeAttentionEngine& operator=(const CascadeAttentionEngine&) = delete;
    CascadeAttentionEngine(CascadeAttentionEngine&&) = delete;
    CascadeAttentionEngine& operator=(CascadeAttentionEngine&&) = delete;

    /**
     * @brief Run reference attention over full concatenated prefix and leaf sequences.
     */
    void forward_reference(
        const float* d_q,
        const float* d_prefix_k,
        const float* d_prefix_v,
        const float* d_leaf_k,
        const float* d_leaf_v,
        float* d_out_ref,
        uint32_t batch_size
    );

    /**
     * @brief Run FlashInfer Cascade Attention (Stage 1 Prefix + Stage 2 Leaf + Stage 3 Fused LSE Rescaling).
     */
    void forward_cascade(
        const float* d_q,
        const float* d_prefix_k,
        const float* d_prefix_v,
        const float* d_leaf_k,
        const float* d_leaf_v,
        float* d_out_cascade,
        uint32_t batch_size
    );

    /**
     * @brief Measure numerical fidelity between reference and cascade outputs.
     */
    void verify_fidelity(
        const float* d_out_ref,
        const float* d_out_cascade,
        uint32_t batch_size,
        double& out_max_abs_err,
        double& out_mse
    );

    /**
     * @brief Benchmark Cascade Attention across batch sizes 1, 2, 4, 8.
     */
    std::vector<CascadeBenchmarkResult> run_benchmark_sweep(int warmups = 5, int iterations = 20);

    [[nodiscard]] const CascadeAttentionConfig& config() const noexcept { return config_; }

private:
    CascadeAttentionConfig config_;

    // Device memory buffers
    float* d_q_{nullptr};
    float* d_prefix_k_{nullptr};
    float* d_prefix_v_{nullptr};
    float* d_leaf_k_{nullptr};
    float* d_leaf_v_{nullptr};
    float* d_out_ref_{nullptr};
    float* d_out_cascade_{nullptr};

    // Intermediate cascade buffers: Stage 1 & Stage 2 outputs and log-sum-exp
    float* d_stage1_out_{nullptr};
    float* d_stage1_m_{nullptr};
    float* d_stage1_d_{nullptr};

    float* d_stage2_out_{nullptr};
    float* d_stage2_m_{nullptr};
    float* d_stage2_d_{nullptr};

    void allocate_device_memory();
    void free_device_memory();
    void initialize_synthetic_data();
};

} // namespace hydrogen
