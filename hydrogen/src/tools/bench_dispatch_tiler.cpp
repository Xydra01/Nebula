// hydrogen/src/tools/bench_dispatch_tiler.cpp
// Diagnostic CLI tool for Issue H09: Templated Dispatch & WDDM Sub-5ms Kernel Clamping

#include "dispatch.hpp"
#include "kernel_tiler.hpp"
#include <iostream>
#include <iomanip>
#include <vector>
#include <string>
#include <filesystem>
#include <chrono>

namespace {

void print_help(const char* prog) {
    std::cout << "Usage: " << prog << " [options]\n\n"
              << "Options:\n"
              << "  --json         Output structured JSON telemetry\n"
              << "  --steps <N>    Number of continuous sweep steps (default: 1000)\n"
              << "  --tokens <N>   Total sequence tokens for tiled test (default: 8192)\n"
              << "  --batch <N>    Concurrent batch size (default: 4)\n"
              << "  --quantum <F>  WDDM clamping quantum threshold in ms (default: 5.0)\n"
              << "  -h, --help     Display this help message\n";
}

const char* quant_type_to_string(hydrogen::PageQuantType q) {
    switch (q) {
        case hydrogen::PageQuantType::FP8_E4M3: return "FP8_E4M3";
        case hydrogen::PageQuantType::Q8_0: return "Q8_0";
        case hydrogen::PageQuantType::FP16: return "FP16";
        default: return "UNKNOWN";
    }
}

} // anonymous namespace

int main(int argc, char* argv[]) {
    bool json_output = false;
    uint32_t steps = 1000;
    uint32_t test_tokens = 8192;
    uint32_t batch_size = 4;
    float max_quantum_ms = 5.0f;

    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--json") {
            json_output = true;
        } else if (arg == "--steps" && i + 1 < argc) {
            steps = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--tokens" && i + 1 < argc) {
            test_tokens = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--batch" && i + 1 < argc) {
            batch_size = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--quantum" && i + 1 < argc) {
            max_quantum_ms = std::stof(argv[++i]);
        } else if (arg == "-h" || arg == "--help") {
            print_help(argv[0]);
            return 0;
        }
    }

    // 1. Forward Scheduler Pre-sorting Test
    std::vector<hydrogen::AttentionRequest> requests = {
        {0, 128, hydrogen::PageQuantType::FP8_E4M3, 1.0f},
        {1, 256, hydrogen::PageQuantType::Q8_0, 1.0f},
        {2, 128, hydrogen::PageQuantType::FP16, 1.0f},
        {3, 512, hydrogen::PageQuantType::FP8_E4M3, 1.0f},
        {4, 128, hydrogen::PageQuantType::Q8_0, 1.0f},
        {5, 256, hydrogen::PageQuantType::FP16, 1.0f}
    };

    auto homogeneous_batches = hydrogen::AttentionDispatcher::presort_batch(requests);

    // 2. Binary metrics inspection
    std::string exe_path = argv[0];
    if (!std::filesystem::exists(exe_path)) {
        if (std::filesystem::exists("hydrogen/build/bin/hydrogen_bench_dispatch_tiler.exe")) {
            exe_path = "hydrogen/build/bin/hydrogen_bench_dispatch_tiler.exe";
        } else if (std::filesystem::exists("build/bin/hydrogen_bench_dispatch_tiler.exe")) {
            exe_path = "build/bin/hydrogen_bench_dispatch_tiler.exe";
        }
    }

    size_t binary_size_bytes = 0;
    double driver_load_ms = 0.0;
    hydrogen::AttentionDispatcher::inspect_binary_metrics(exe_path, binary_size_bytes, driver_load_ms);
    double binary_size_mb = static_cast<double>(binary_size_bytes) / (1024.0 * 1024.0);

    bool binary_size_passed = (binary_size_mb < 15.0);
    bool driver_load_passed = (driver_load_ms < 200.0);

    // 3. WDDM Kernel Tiler Execution & Sweep
    hydrogen::WddmClampingConfig config;
    config.max_quantum_ms = max_quantum_ms;
    config.max_seq_chunk = 1024;
    config.max_batch_size = 16;
    config.head_dim = 128;
    config.num_q_heads = 32;
    config.num_kv_heads = 8;

    hydrogen::WddmTilerBenchmarkResult sweep_result;

    try {
        hydrogen::WddmKernelTiler tiler(config);

        // Verification of micro-chunk partitioning
        auto test_tiles = tiler.tile_workload(test_tokens);
        bool partitioning_valid = true;
        for (const auto& tile : test_tiles) {
            if (tile.num_tokens > config.max_seq_chunk) {
                partitioning_valid = false;
                break;
            }
        }
        (void)partitioning_valid;

        // Single execution of clamped workload
        float single_max_ms = tiler.execute_clamped_workload(test_tokens, batch_size, true);
        (void)single_max_ms;

        // Comprehensive multi-step sweep
        sweep_result = tiler.run_clamping_sweep(steps);

    } catch (const std::exception& e) {
        std::cerr << "Fatal Error during WDDM kernel tiler execution: " << e.what() << std::endl;
        return 1;
    }

    bool warp_efficiency_passed = (sweep_result.templated_warp_efficiency >= 95.0);
    bool wddm_clamping_passed = (sweep_result.clamping_invariant_held && sweep_result.preemption_stalls == 0);
    bool all_passed = warp_efficiency_passed && binary_size_passed && driver_load_passed && wddm_clamping_passed;

    if (json_output) {
        std::cout << "{\n"
                  << "  \"status\": \"" << (all_passed ? "PASS" : "FAIL") << "\",\n"
                  << "  \"warp_efficiency_passed\": " << (warp_efficiency_passed ? "true" : "false") << ",\n"
                  << "  \"binary_size_passed\": " << (binary_size_passed ? "true" : "false") << ",\n"
                  << "  \"driver_load_passed\": " << (driver_load_passed ? "true" : "false") << ",\n"
                  << "  \"wddm_clamping_passed\": " << (wddm_clamping_passed ? "true" : "false") << ",\n"
                  << "  \"metrics\": {\n"
                  << "    \"divergent_warp_efficiency_pct\": " << std::fixed << std::setprecision(1) << sweep_result.divergent_warp_efficiency << ",\n"
                  << "    \"templated_warp_efficiency_pct\": " << std::fixed << std::setprecision(1) << sweep_result.templated_warp_efficiency << ",\n"
                  << "    \"speedup_ratio\": " << std::fixed << std::setprecision(2) << sweep_result.speedup_ratio << ",\n"
                  << "    \"binary_size_bytes\": " << binary_size_bytes << ",\n"
                  << "    \"binary_size_mb\": " << std::fixed << std::setprecision(2) << binary_size_mb << ",\n"
                  << "    \"driver_load_time_ms\": " << std::fixed << std::setprecision(2) << driver_load_ms << ",\n"
                  << "    \"max_kernel_duration_ms\": " << std::fixed << std::setprecision(3) << sweep_result.max_kernel_duration_ms << ",\n"
                  << "    \"avg_kernel_duration_ms\": " << std::fixed << std::setprecision(3) << sweep_result.avg_kernel_duration_ms << ",\n"
                  << "    \"min_kernel_duration_ms\": " << std::fixed << std::setprecision(3) << sweep_result.min_kernel_duration_ms << ",\n"
                  << "    \"preemption_stalls\": " << sweep_result.preemption_stalls << ",\n"
                  << "    \"total_tiles_launched\": " << sweep_result.total_tiles_launched << ",\n"
                  << "    \"total_steps\": " << sweep_result.total_steps << "\n"
                  << "  },\n"
                  << "  \"presort_batch\": {\n"
                  << "    \"input_requests\": " << requests.size() << ",\n"
                  << "    \"homogeneous_subbatches\": " << homogeneous_batches.size() << ",\n"
                  << "    \"batches\": [\n";

        for (size_t b = 0; b < homogeneous_batches.size(); ++b) {
            const auto& batch = homogeneous_batches[b];
            std::cout << "      {\n"
                      << "        \"quant_type\": \"" << quant_type_to_string(batch.quant_type) << "\",\n"
                      << "        \"request_count\": " << batch.request_ids.size() << ",\n"
                      << "        \"total_tokens\": " << batch.total_tokens << "\n"
                      << "      }" << (b + 1 < homogeneous_batches.size() ? "," : "") << "\n";
        }

        std::cout << "    ]\n"
                  << "  }\n"
                  << "}\n";
    } else {
        std::cout << "\n========================================================================\n"
                  << "Project Hydrogen: Templated Dispatch & WDDM Kernel Clamping SM 8.9\n"
                  << "========================================================================\n\n";

        std::cout << "1. Forward Scheduler Pre-Sorting Demonstration:\n"
                  << "------------------------------------------------------------------------\n"
                  << "Input requests: " << requests.size() << " (mixed FP8_E4M3, Q8_0, FP16)\n";
        for (const auto& b : homogeneous_batches) {
            std::cout << "  - Sub-batch [" << std::setw(8) << quant_type_to_string(b.quant_type) << "]: "
                      << b.request_ids.size() << " requests, "
                      << b.total_tokens << " total tokens\n";
        }
        std::cout << "\n";

        std::cout << "2. Warp Execution Efficiency Benchmark (Divergent vs Templated):\n"
                  << "------------------------------------------------------------------------\n"
                  << "  - Baseline (Divergent inner loop):   " << std::fixed << std::setprecision(1)
                  << sweep_result.divergent_warp_efficiency << "% warp execution efficiency\n"
                  << "  - Templated (Branch-free inner loop): " << std::fixed << std::setprecision(1)
                  << sweep_result.templated_warp_efficiency << "% warp execution efficiency\n"
                  << "  - Speedup Ratio:                     " << std::fixed << std::setprecision(2)
                  << sweep_result.speedup_ratio << "x\n"
                  << "  - Warp Efficiency Requirement (>95%): " << (warp_efficiency_passed ? "[PASS]" : "[FAIL]") << "\n\n";

        std::cout << "3. Binary Size & Driver Initialization Latency:\n"
                  << "------------------------------------------------------------------------\n"
                  << "  - Executable Path:            " << exe_path << "\n"
                  << "  - Binary File Size:           " << std::fixed << std::setprecision(2)
                  << binary_size_mb << " MB (" << binary_size_bytes << " bytes)\n"
                  << "  - Binary Size Threshold:      < 15.0 MB " << (binary_size_passed ? "[PASS]" : "[FAIL]") << "\n"
                  << "  - CUDA Driver Init Latency:   " << std::fixed << std::setprecision(2)
                  << driver_load_ms << " ms\n"
                  << "  - Driver Init Threshold:      < 200.0 ms " << (driver_load_passed ? "[PASS]" : "[FAIL]") << "\n\n";

        std::cout << "4. WDDM Sub-5ms Clamping Invariant Sweep (" << sweep_result.total_steps << " steps):\n"
                  << "------------------------------------------------------------------------\n"
                  << "  - Total Micro-Tiles Launched: " << sweep_result.total_tiles_launched << "\n"
                  << "  - Min Kernel Duration:        " << std::fixed << std::setprecision(3)
                  << sweep_result.min_kernel_duration_ms << " ms\n"
                  << "  - Avg Kernel Duration:        " << std::fixed << std::setprecision(3)
                  << sweep_result.avg_kernel_duration_ms << " ms\n"
                  << "  - Max Kernel Duration:        " << std::fixed << std::setprecision(3)
                  << sweep_result.max_kernel_duration_ms << " ms\n"
                  << "  - Clamping Threshold:         < " << std::fixed << std::setprecision(1) << max_quantum_ms << " ms\n"
                  << "  - WDDM Preemption Stalls:     " << sweep_result.preemption_stalls << " stalls\n"
                  << "  - Sub-5ms Invariant Met:      " << (wddm_clamping_passed ? "[PASS]" : "[FAIL]") << "\n\n";

        std::cout << "========================================================================\n"
                  << "Final Status: " << (all_passed ? "ALL CHECKS PASSED [SUCCESS]" : "CHECKS FAILED [FAILURE]") << "\n"
                  << "========================================================================\n\n";
    }

    return all_passed ? 0 : 1;
}
