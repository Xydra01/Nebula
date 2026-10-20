#pragma once

#include <cstdint>
#include <cstddef>
#include <vector>
#include <span>

namespace hydrogen {

/**
 * @brief Configuration parameters for Marlin Quantized GEMM evaluation.
 */
struct MarlinGemmConfig {
    uint32_t M = 8;             // Batch size / sequence token count (e.g. 1, 2, 4, 8)
    uint32_t K = 4096;          // Input dimension (reduction dimension)
    uint32_t N = 4096;          // Output dimension
    uint32_t group_size = 128;  // Quantization group size (e.g. 128 or 256 for Q4_K)
};

/**
 * @brief Performance and numerical accuracy metrics for Marlin GEMM.
 */
struct MarlinBenchmarkResult {
    uint32_t M = 1;
    uint32_t K = 4096;
    uint32_t N = 4096;
    float marlin_latency_ms = 0.0f;
    float baseline_latency_ms = 0.0f;
    double marlin_tflops = 0.0;
    double marlin_bandwidth_gbs = 0.0;
    double bandwidth_utilization_pct = 0.0; // vs 504.0 GB/s on RTX 4070
    double speedup_ratio = 1.0;
    double max_abs_error = 0.0;
    double mean_squared_error = 0.0;
    bool numerical_match = true;      // max_abs_error < 1e-3
    bool latency_target_met = true;  // <= 0.15 ms
    bool bandwidth_target_met = true; // > 80% of 504 GB/s (> 403.2 GB/s)
};

/**
 * @brief Diagnostic telemetry for GGUF Q4_K to Marlin layout repacking.
 */
struct MarlinRepackResult {
    size_t num_weights = 0;
    double repack_time_ms = 0.0;
    double repack_time_seconds = 0.0;
    bool repack_under_1s = true;
};

/**
 * @brief Marlin / CUTLASS Quantized GEMM Pipeline Engine.
 * 
 * Provides:
 * 1. Weight repacking from GGUF Q4_K to Marlin 4-bit tile-interleave format.
 * 2. Asynchronous cp.async double-buffered 4-bit GEMM for attention projections.
 * 3. Hardware-accelerated INT8 matrix multiplication for MLP projections.
 * 4. Un-optimized baseline INT4 GEMM for empirical speedup verification.
 */
class MarlinGemmEngine {
public:
    explicit MarlinGemmEngine(MarlinGemmConfig config);
    ~MarlinGemmEngine();

    // Non-copyable, non-movable
    MarlinGemmEngine(const MarlinGemmEngine&) = delete;
    MarlinGemmEngine& operator=(const MarlinGemmEngine&) = delete;
    MarlinGemmEngine(MarlinGemmEngine&&) = delete;
    MarlinGemmEngine& operator=(MarlinGemmEngine&&) = delete;

    /**
     * @brief Repack standard linear / GGUF Q4_K weights into Marlin tile-interleaved format.
     * @param h_src_q4k Host pointer to source linear 4-bit weights
     * @param h_dst_marlin Host pointer to destination Marlin packed buffer
     * @param K Matrix input dimension
     * @param N Matrix output dimension
     * @return Telemetry with elapsed repacking time
     */
    MarlinRepackResult repack_q4k_to_marlin(
        const uint8_t* h_src_q4k,
        uint32_t* h_dst_marlin,
        uint32_t K,
        uint32_t N
    );

    /**
     * @brief Run un-optimized baseline linear INT4 GEMM.
     */
    void forward_baseline_int4(
        const float* d_X,
        const uint32_t* d_W_linear,
        const float* d_scales,
        const float* d_biases,
        float* d_Y,
        uint32_t M
    );

    /**
     * @brief Run Marlin 4-bit GEMM with asynchronous cp.async double-buffering.
     */
    void forward_marlin_int4(
        const float* d_X,
        const uint32_t* d_W_marlin,
        const float* d_scales,
        const float* d_biases,
        float* d_Y,
        uint32_t M
    );

    /**
     * @brief Run CUTLASS-style INT8 GEMM for MLP projection layers.
     */
    void forward_cutlass_int8(
        const float* d_X,
        const int8_t* d_W_int8,
        const float* d_scales,
        float* d_Y,
        uint32_t M
    );

    /**
     * @brief Reference full-precision dequantized GEMM for numerical verification.
     */
    void forward_reference(
        const float* d_X,
        const float* d_W_dequant,
        float* d_Y_ref,
        uint32_t M
    );

    /**
     * @brief Measure numerical fidelity between reference and Marlin outputs.
     */
    void verify_fidelity(
        const float* d_Y_ref,
        const float* d_Y_marlin,
        uint32_t M,
        double& out_max_abs_err,
        double& out_mse
    );

    /**
     * @brief Benchmark Marlin GEMM across M in {1, 2, 4, 8} with K=4096, N=4096.
     */
    std::vector<MarlinBenchmarkResult> run_benchmark_sweep(int warmups = 5, int iterations = 20);

    /**
     * @brief Benchmark GGUF Q4_K weight repacking performance.
     */
    MarlinRepackResult benchmark_repacking();

    [[nodiscard]] const MarlinGemmConfig& config() const noexcept { return config_; }

private:
    MarlinGemmConfig config_;

    // Device memory buffers
    float* d_X_{nullptr};           // [M_max, K]
    uint32_t* d_W_linear_{nullptr}; // [K, N / 8] packed 4-bit linear
    uint32_t* d_W_marlin_{nullptr}; // [K / 16, N * 16 / 8] Marlin interleaved
    int8_t* d_W_int8_{nullptr};     // [K, N] 8-bit quantized weights
    float* d_scales_{nullptr};      // [K / group_size, N] scales
    float* d_biases_{nullptr};      // [K / group_size, N] biases
    float* d_W_dequant_{nullptr};   // [K, N] full precision reference weights
    float* d_Y_ref_{nullptr};       // [M_max, N] reference output
    float* d_Y_marlin_{nullptr};    // [M_max, N] Marlin output
    float* d_Y_baseline_{nullptr};  // [M_max, N] Baseline output
    float* d_Y_cutlass_{nullptr};   // [M_max, N] CUTLASS INT8 output

    void allocate_device_memory();
    void free_device_memory();
    void initialize_synthetic_data();
};

} // namespace hydrogen
