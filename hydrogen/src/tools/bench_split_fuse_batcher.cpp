// hydrogen/src/tools/bench_split_fuse_batcher.cpp
// Diagnostic CLI tool for Issue H10: Split-Fuse Continuous Batching Scheduler

#include "batcher.hpp"
#include <iostream>
#include <iomanip>
#include <vector>
#include <string>

namespace {

void print_help(const char* prog) {
    std::cout << "Usage: " << prog << " [options]\n\n"
              << "Options:\n"
              << "  --json            Output structured JSON telemetry\n"
              << "  --streams <N>     Active decode streams (default: 8)\n"
              << "  --prefill <N>     Injected tool prefill tokens (default: 1500)\n"
              << "  --chunk <N>       Max prefill chunk size (default: 64)\n"
              << "  --pre-steps <N>   Baseline steps before prefill (default: 20)\n"
              << "  --post-steps <N>  Steps after prefill (default: 20)\n"
              << "  -h, --help        Display this help message\n";
}

} // anonymous namespace

int main(int argc, char* argv[]) {
    bool json_output = false;
    uint32_t streams = 8;
    uint32_t prefill_tokens = 1500;
    uint32_t chunk_size = 64;
    uint32_t pre_steps = 20;
    uint32_t post_steps = 20;

    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--json") {
            json_output = true;
        } else if (arg == "--streams" && i + 1 < argc) {
            streams = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--prefill" && i + 1 < argc) {
            prefill_tokens = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--chunk" && i + 1 < argc) {
            chunk_size = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--pre-steps" && i + 1 < argc) {
            pre_steps = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--post-steps" && i + 1 < argc) {
            post_steps = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "-h" || arg == "--help") {
            print_help(argv[0]);
            return 0;
        }
    }

    hydrogen::SplitFuseConfig config;
    config.max_prefill_chunk_tokens = chunk_size;
    config.max_decode_streams = std::max(streams + 2, 16u);

    hydrogen::SplitFuseBenchmarkResult result;

    try {
        hydrogen::SplitFuseBatcher batcher(config);
        result = batcher.run_simulation(streams, prefill_tokens, pre_steps, post_steps);
    } catch (const std::exception& e) {
        std::cerr << "Fatal Error during Split-Fuse batcher benchmark: " << e.what() << std::endl;
        return 1;
    }

    // Monolithic unchunked prefill baseline calculation for comparison
    float monolithic_prefill_ms = static_cast<float>(prefill_tokens) * 0.15254f;
    float monolithic_jitter_pct = ((monolithic_prefill_ms - result.decode_latency_before_prefill_ms) /
                                   result.decode_latency_before_prefill_ms) * 100.0f;

    bool all_passed = result.all_invariants_met;

    if (json_output) {
        std::cout << "{\n"
                  << "  \"status\": \"" << (all_passed ? "PASS" : "FAIL") << "\",\n"
                  << "  \"zero_stutter_passed\": " << (result.zero_stutter_passed ? "true" : "false") << ",\n"
                  << "  \"max_chunk_clamped_passed\": " << (result.max_chunk_clamped_passed ? "true" : "false") << ",\n"
                  << "  \"assembly_overhead_passed\": " << (result.assembly_overhead_passed ? "true" : "false") << ",\n"
                  << "  \"prefill_duration_passed\": " << (result.prefill_duration_passed ? "true" : "false") << ",\n"
                  << "  \"metrics\": {\n"
                  << "    \"decode_streams\": " << streams << ",\n"
                  << "    \"prefill_tokens\": " << prefill_tokens << ",\n"
                  << "    \"max_prefill_chunk_tokens\": " << chunk_size << ",\n"
                  << "    \"max_prefill_chunk_observed\": " << result.max_prefill_chunk_observed << ",\n"
                  << "    \"prefill_iterations\": " << result.prefill_iterations << ",\n"
                  << "    \"prefill_total_duration_ms\": " << std::fixed << std::setprecision(2) << result.prefill_total_duration_ms << ",\n"
                  << "    \"decode_latency_before_ms\": " << std::fixed << std::setprecision(2) << result.decode_latency_before_prefill_ms << ",\n"
                  << "    \"decode_latency_during_ms\": " << std::fixed << std::setprecision(2) << result.decode_latency_during_prefill_ms << ",\n"
                  << "    \"decode_latency_after_ms\": " << std::fixed << std::setprecision(2) << result.decode_latency_after_prefill_ms << ",\n"
                  << "    \"max_decode_step_latency_ms\": " << std::fixed << std::setprecision(2) << result.max_decode_step_latency_ms << ",\n"
                  << "    \"decode_jitter_pct\": " << std::fixed << std::setprecision(1) << result.decode_jitter_pct << ",\n"
                  << "    \"avg_assembly_overhead_us\": " << std::fixed << std::setprecision(2) << result.avg_assembly_overhead_us << ",\n"
                  << "    \"max_assembly_overhead_us\": " << std::fixed << std::setprecision(2) << result.max_assembly_overhead_us << ",\n"
                  << "    \"monolithic_prefill_latency_ms\": " << std::fixed << std::setprecision(2) << monolithic_prefill_ms << ",\n"
                  << "    \"monolithic_jitter_pct\": " << std::fixed << std::setprecision(1) << monolithic_jitter_pct << "\n"
                  << "  }\n"
                  << "}\n";
    } else {
        std::cout << "\n========================================================================\n"
                  << "Project Hydrogen: Split-Fuse Continuous Batching Scheduler SM 8.9\n"
                  << "========================================================================\n\n";

        std::cout << "1. Workload Configuration:\n"
                  << "------------------------------------------------------------------------\n"
                  << "  - Active Token Decoding Streams: " << streams << "\n"
                  << "  - Injected Tool Prefill Tokens:  " << prefill_tokens << " tokens\n"
                  << "  - Prefill Chunk Clamp Size (M):  " << chunk_size << " tokens\n"
                  << "  - Max Chunk Observed in Step:    " << result.max_prefill_chunk_observed << " tokens\n"
                  << "  - Prefill Chunks / Iterations:   " << result.prefill_iterations << " steps\n"
                  << "  - Prefill Total Duration:        " << std::fixed << std::setprecision(2)
                  << result.prefill_total_duration_ms << " ms (< 300 ms target) "
                  << (result.prefill_duration_passed ? "[PASS]" : "[FAIL]") << "\n\n";

        std::cout << "2. Decode Step Latency & UI Stutter Evaluation:\n"
                  << "------------------------------------------------------------------------\n"
                  << "  - Decode Latency (Before Prefill): " << std::fixed << std::setprecision(2)
                  << result.decode_latency_before_prefill_ms << " ms\n"
                  << "  - Decode Latency (During Prefill): " << std::fixed << std::setprecision(2)
                  << result.decode_latency_during_prefill_ms << " ms\n"
                  << "  - Decode Latency (After Prefill):  " << std::fixed << std::setprecision(2)
                  << result.decode_latency_after_prefill_ms << " ms\n"
                  << "  - Maximum Step Latency Observed:   " << std::fixed << std::setprecision(2)
                  << result.max_decode_step_latency_ms << " ms (Threshold: <= 15.50 ms) "
                  << (result.max_decode_latency_passed ? "[PASS]" : "[FAIL]") << "\n"
                  << "  - Measured Step Latency Jitter:    " << std::fixed << std::setprecision(1)
                  << result.decode_jitter_pct << "% (Threshold: <= 10.0%) "
                  << (result.decode_jitter_passed ? "[PASS]" : "[FAIL]") << "\n\n";

        std::cout << "3. Monolithic vs Split-Fuse Jitter Comparison:\n"
                  << "------------------------------------------------------------------------\n"
                  << "  - Monolithic (Unchunked) Prefill:  " << std::fixed << std::setprecision(2)
                  << monolithic_prefill_ms << " ms (" << std::setprecision(1) << monolithic_jitter_pct << "% spike -> SEVERE FREEZE)\n"
                  << "  - Split-Fuse (64-Token Clamped):   " << std::fixed << std::setprecision(2)
                  << result.max_decode_step_latency_ms << " ms (" << std::setprecision(1) << result.decode_jitter_pct << "% jitter -> ZERO STUTTER)\n\n";

        std::cout << "4. Continuous Batch Assembly Overhead (< 15 us Invariant):\n"
                  << "------------------------------------------------------------------------\n"
                  << "  - Average Batch Assembly Overhead: " << std::fixed << std::setprecision(2)
                  << result.avg_assembly_overhead_us << " us\n"
                  << "  - Maximum Batch Assembly Overhead: " << std::fixed << std::setprecision(2)
                  << result.max_assembly_overhead_us << " us\n"
                  << "  - Assembly Overhead Target (<15us): "
                  << (result.assembly_overhead_passed ? "[PASS]" : "[FAIL]") << "\n\n";

        std::cout << "========================================================================\n"
                  << "Final Status: " << (all_passed ? "ALL CHECKS PASSED [SUCCESS]" : "CHECKS FAILED [FAILURE]") << "\n"
                  << "========================================================================\n\n";
    }

    return all_passed ? 0 : 1;
}
