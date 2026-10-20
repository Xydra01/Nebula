// hydrogen/src/tools/bench_marlin_gemm.cpp
// Diagnostic and benchmark utility for Issue H08: Marlin & CUTLASS Quantized GEMM Pipeline

#include "marlin_gemm.hpp"
#include <iostream>
#include <iomanip>
#include <vector>
#include <string>

namespace {

void print_help(const char* prog) {
    std::cout << "Usage: " << prog << " [options]\n\n"
              << "Options:\n"
              << "  --json         Output structured JSON telemetry\n"
              << "  --k <N>        Matrix dimension K (default: 4096)\n"
              << "  --n <N>        Matrix dimension N (default: 4096)\n"
              << "  --group <N>    Quantization group size (default: 128)\n"
              << "  --iters <N>    Timed benchmark iterations (default: 20)\n"
              << "  --warmups <N>  Warmup iterations (default: 5)\n"
              << "  -h, --help     Display this help message\n";
}

} // anonymous namespace

int main(int argc, char* argv[]) {
    bool json_output = false;
    uint32_t K = 4096;
    uint32_t N = 4096;
    uint32_t group_size = 128;
    int warmups = 5;
    int iters = 20;

    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--json") {
            json_output = true;
        } else if (arg == "--k" && i + 1 < argc) {
            K = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--n" && i + 1 < argc) {
            N = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--group" && i + 1 < argc) {
            group_size = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--iters" && i + 1 < argc) {
            iters = std::stoi(argv[++i]);
        } else if (arg == "--warmups" && i + 1 < argc) {
            warmups = std::stoi(argv[++i]);
        } else if (arg == "-h" || arg == "--help") {
            print_help(argv[0]);
            return 0;
        }
    }

    hydrogen::MarlinGemmConfig config;
    config.M = 8;
    config.K = K;
    config.N = N;
    config.group_size = group_size;

    std::vector<hydrogen::MarlinBenchmarkResult> results;
    hydrogen::MarlinRepackResult repack_result;

    try {
        hydrogen::MarlinGemmEngine engine(config);
        repack_result = engine.benchmark_repacking();
        results = engine.run_benchmark_sweep(warmups, iters);
    } catch (const std::exception& e) {
        std::cerr << "Fatal Error: " << e.what() << std::endl;
        return 1;
    }

    bool all_numerical_passed = true;
    bool all_latency_passed = true;
    bool all_speedup_passed = true;
    bool peak_bandwidth_passed = false;

    for (const auto& r : results) {
        if (!r.numerical_match) all_numerical_passed = false;
        if (!r.latency_target_met) all_latency_passed = false;
        if (r.speedup_ratio < 1.8) all_speedup_passed = false;
        if (r.bandwidth_utilization_pct >= 80.0) peak_bandwidth_passed = true;
    }

    bool repack_passed = repack_result.repack_under_1s;
    bool check_bandwidth = (K >= 4096 && N >= 4096);
    bool all_passed = repack_passed && all_numerical_passed && all_latency_passed && all_speedup_passed &&
                      (!check_bandwidth || peak_bandwidth_passed);

    if (json_output) {
        std::cout << "{\n"
                  << "  \"status\": \"" << (all_passed ? "PASS" : "FAIL") << "\",\n"
                  << "  \"K\": " << K << ",\n"
                  << "  \"N\": " << N << ",\n"
                  << "  \"group_size\": " << group_size << ",\n"
                  << "  \"repack_time_ms\": " << std::fixed << std::setprecision(2) << repack_result.repack_time_ms << ",\n"
                  << "  \"repack_under_1s\": " << (repack_passed ? "true" : "false") << ",\n"
                  << "  \"numerical_match_passed\": " << (all_numerical_passed ? "true" : "false") << ",\n"
                  << "  \"latency_le_0_15ms_passed\": " << (all_latency_passed ? "true" : "false") << ",\n"
                  << "  \"speedup_gt_1_8x_passed\": " << (all_speedup_passed ? "true" : "false") << ",\n"
                  << "  \"bandwidth_gt_80pct_passed\": " << (peak_bandwidth_passed ? "true" : "false") << ",\n"
                  << "  \"sweep\": [\n";

        for (size_t i = 0; i < results.size(); ++i) {
            const auto& r = results[i];
            std::cout << "    {\n"
                      << "      \"M\": " << r.M << ",\n"
                      << "      \"marlin_latency_ms\": " << std::fixed << std::setprecision(3) << r.marlin_latency_ms << ",\n"
                      << "      \"baseline_latency_ms\": " << std::fixed << std::setprecision(3) << r.baseline_latency_ms << ",\n"
                      << "      \"speedup_ratio\": " << std::fixed << std::setprecision(2) << r.speedup_ratio << ",\n"
                      << "      \"bandwidth_gbs\": " << std::fixed << std::setprecision(1) << r.marlin_bandwidth_gbs << ",\n"
                      << "      \"bandwidth_util_pct\": " << std::fixed << std::setprecision(1) << r.bandwidth_utilization_pct << ",\n"
                      << "      \"tflops\": " << std::fixed << std::setprecision(2) << r.marlin_tflops << ",\n"
                      << "      \"max_abs_error\": " << std::scientific << r.max_abs_error << ",\n"
                      << "      \"mse\": " << std::scientific << r.mean_squared_error << "\n"
                      << "    }" << (i + 1 < results.size() ? "," : "") << "\n";
        }
        std::cout << "  ]\n}\n";
    } else {
        std::cout << "\n===================================================\n"
                  << "Project Hydrogen: Marlin & CUTLASS Quantized GEMM SM 8.9\n"
                  << "===================================================\n"
                  << "Matrix Geometry:           K=" << K << ", N=" << N << " (" << (K * N) / (1024 * 1024) << "M params)\n"
                  << "Quantization Format:       4-bit (Marlin interleaved) | Group size=" << group_size << "\n"
                  << "Startup Weight Repacking:  " << std::fixed << std::setprecision(2) << repack_result.repack_time_ms << " ms "
                  << "(" << (repack_passed ? "PASS < 1.0s" : "FAIL") << ")\n"
                  << "Peak Bus Theoretical:      504.0 GB/s (RTX 4070 12GB 192-bit GDDR6X)\n"
                  << "---------------------------------------------------\n"
                  << "Batch Sweep Results:\n"
                  << "   M   | Marlin (ms) | Baseline (ms) | Speedup | Bandwidth | Bus Util | Max Abs Err | Status\n"
                  << "  -----+-------------+---------------+---------+-----------+----------+-------------+-------\n";

        for (const auto& r : results) {
            std::cout << "  M=" << std::setw(2) << r.M << " | "
                      << std::setw(8) << std::fixed << std::setprecision(3) << r.marlin_latency_ms << " ms | "
                      << std::setw(10) << std::fixed << std::setprecision(3) << r.baseline_latency_ms << " ms | "
                      << std::setw(5) << std::fixed << std::setprecision(2) << r.speedup_ratio << "x  | "
                      << std::setw(6) << std::fixed << std::setprecision(1) << r.marlin_bandwidth_gbs << " GB/s | "
                      << std::setw(5) << std::fixed << std::setprecision(1) << r.bandwidth_utilization_pct << "%  | "
                      << std::scientific << std::setprecision(2) << r.max_abs_error << "  | "
                      << (r.numerical_match && r.latency_target_met && r.speedup_ratio >= 1.8 ? "PASS" : "FAIL") << "\n";
        }

        std::cout << "---------------------------------------------------\n"
                  << "Acceptance Criteria Verification:\n"
                  << "  Weight Repacking (<1.0s):  " << (repack_passed ? "PASS" : "FAIL") << " (" << std::fixed << std::setprecision(2) << repack_result.repack_time_ms << " ms)\n"
                  << "  GEMM Speedup (>1.8x):      " << (all_speedup_passed ? "PASS (> 1.8x vs cuBLASLt baseline)" : "FAIL") << "\n"
                  << "  Execution Time (<=0.15ms): " << (all_latency_passed ? "PASS (<= 0.15 ms per GEMM)" : "FAIL") << "\n"
                  << "  Memory Bus Saturation (>80%): " << (peak_bandwidth_passed ? "PASS (> 80% theoretical 504 GB/s during weight reads)" : "FAIL") << "\n"
                  << "  Numerical Accuracy (<1e-3):   " << (all_numerical_passed ? "PASS (< 1e-3 tolerance)" : "FAIL") << "\n"
                  << "===================================================\n"
                  << "Overall Status:            " << (all_passed ? "PASS (All Invariants Verified)" : "FAIL") << "\n\n";
    }

    return all_passed ? 0 : 1;
}
