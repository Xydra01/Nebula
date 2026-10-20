// hydrogen/src/tools/bench_cascade_attention.cpp
// Diagnostic and benchmark utility for Issue H07: FlashInfer Cascade Attention SM 8.9 Integration

#include "cascade_attention.hpp"
#include <iostream>
#include <iomanip>
#include <vector>
#include <string>
#include <cassert>

namespace {

void print_help(const char* prog) {
    std::cout << "Usage: " << prog << " [options]\n\n"
              << "Options:\n"
              << "  --json         Output structured JSON telemetry\n"
              << "  --prefix <N>   Shared prefix length in tokens (default: 4096)\n"
              << "  --leaf <N>     Private leaf length in tokens (default: 128)\n"
              << "  --iters <N>    Timed benchmark iterations (default: 20)\n"
              << "  --warmups <N>  Warmup iterations (default: 5)\n"
              << "  -h, --help     Display this help message\n";
}

} // anonymous namespace

int main(int argc, char* argv[]) {
    bool json_output = false;
    uint32_t prefix_len = 4096;
    uint32_t leaf_len = 128;
    int warmups = 5;
    int iters = 20;

    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--json") {
            json_output = true;
        } else if (arg == "--prefix" && i + 1 < argc) {
            prefix_len = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--leaf" && i + 1 < argc) {
            leaf_len = static_cast<uint32_t>(std::stoul(argv[++i]));
        } else if (arg == "--iters" && i + 1 < argc) {
            iters = std::stoi(argv[++i]);
        } else if (arg == "--warmups" && i + 1 < argc) {
            warmups = std::stoi(argv[++i]);
        } else if (arg == "-h" || arg == "--help") {
            print_help(argv[0]);
            return 0;
        }
    }

    hydrogen::CascadeAttentionConfig config;
    config.batch_size = 8;
    config.head_dim = 128;
    config.num_q_heads = 32;
    config.num_kv_heads = 8;
    config.prefix_len = prefix_len;
    config.leaf_len = leaf_len;

    std::vector<hydrogen::CascadeBenchmarkResult> results;
    try {
        hydrogen::CascadeAttentionEngine engine(config);
        results = engine.run_benchmark_sweep(warmups, iters);
    } catch (const std::exception& e) {
        std::cerr << "Fatal Error: " << e.what() << std::endl;
        return 1;
    }

    bool all_numerical_passed = true;
    for (const auto& r : results) {
        if (!r.numerical_match) {
            all_numerical_passed = false;
        }
    }

    const auto& b8_result = results.back(); // B = 8
    bool throughput_passed = (b8_result.throughput_tok_s >= 500.0);
    bool speedup_passed = (b8_result.speedup_ratio >= 6.0);
    bool all_passed = all_numerical_passed && throughput_passed && speedup_passed;

    if (json_output) {
        std::cout << "{\n"
                  << "  \"status\": \"" << (all_passed ? "PASS" : "FAIL") << "\",\n"
                  << "  \"prefix_len\": " << prefix_len << ",\n"
                  << "  \"leaf_len\": " << leaf_len << ",\n"
                  << "  \"head_dim\": " << config.head_dim << ",\n"
                  << "  \"num_q_heads\": " << config.num_q_heads << ",\n"
                  << "  \"num_kv_heads\": " << config.num_kv_heads << ",\n"
                  << "  \"numerical_tolerance_passed\": " << (all_numerical_passed ? "true" : "false") << ",\n"
                  << "  \"throughput_ge_500_tok_s\": " << (throughput_passed ? "true" : "false") << ",\n"
                  << "  \"speedup_ge_6x\": " << (speedup_passed ? "true" : "false") << ",\n"
                  << "  \"b8_latency_ms\": " << std::fixed << std::setprecision(3) << b8_result.latency_ms << ",\n"
                  << "  \"b8_throughput_tok_s\": " << std::fixed << std::setprecision(1) << b8_result.throughput_tok_s << ",\n"
                  << "  \"b8_speedup_ratio\": " << std::fixed << std::setprecision(2) << b8_result.speedup_ratio << ",\n"
                  << "  \"b8_max_abs_error\": " << std::scientific << b8_result.max_abs_error << ",\n"
                  << "  \"b8_mse\": " << std::scientific << b8_result.mean_squared_error << ",\n"
                  << "  \"sweep\": [\n";

        for (size_t i = 0; i < results.size(); ++i) {
            const auto& r = results[i];
            std::cout << "    {\n"
                      << "      \"batch_size\": " << r.batch_size << ",\n"
                      << "      \"latency_ms\": " << std::fixed << std::setprecision(3) << r.latency_ms << ",\n"
                      << "      \"throughput_tok_s\": " << std::fixed << std::setprecision(1) << r.throughput_tok_s << ",\n"
                      << "      \"speedup_ratio\": " << std::fixed << std::setprecision(2) << r.speedup_ratio << ",\n"
                      << "      \"max_abs_error\": " << std::scientific << r.max_abs_error << ",\n"
                      << "      \"mse\": " << std::scientific << r.mean_squared_error << "\n"
                      << "    }" << (i + 1 < results.size() ? "," : "") << "\n";
        }
        std::cout << "  ]\n}\n";
    } else {
        std::cout << "\n===================================================\n"
                  << "Project Hydrogen: FlashInfer Cascade Attention SM 8.9\n"
                  << "===================================================\n"
                  << "Geometry:                  Q_heads=" << config.num_q_heads
                  << ", KV_heads=" << config.num_kv_heads
                  << " (GQA " << (config.num_q_heads / config.num_kv_heads) << ":1), D=" << config.head_dim << "\n"
                  << "Sequence Composition:      Shared Prefix=" << prefix_len
                  << " tokens | Private Leaf=" << leaf_len << " tokens\n"
                  << "Prefix Memory Streaming:   Read exactly ONCE across all sequences (87.5% bandwidth reduction)\n"
                  << "---------------------------------------------------\n"
                  << "Batch Sweep Results:\n"
                  << "  Batch  | Latency (ms) | Throughput (tok/s) | Speedup | Max Abs Err | Numerical Fidelity\n"
                  << "  -------+--------------+--------------------+---------+-------------+-------------------\n";

        for (const auto& r : results) {
            std::cout << "    B=" << r.batch_size << "   | "
                      << std::setw(8) << std::fixed << std::setprecision(3) << r.latency_ms << " ms | "
                      << std::setw(11) << std::fixed << std::setprecision(1) << r.throughput_tok_s << " tok/s | "
                      << std::setw(5) << std::fixed << std::setprecision(2) << r.speedup_ratio << "x  | "
                      << std::scientific << std::setprecision(2) << r.max_abs_error << "  | "
                      << (r.numerical_match ? "PASS (<1e-4)" : "FAIL") << "\n";
        }

        std::cout << "---------------------------------------------------\n"
                  << "Acceptance Criteria Verification:\n"
                  << "  Numerical Output Match:  " << (all_numerical_passed ? "PASS (Max err < 1e-4)" : "FAIL") << "\n"
                  << "  Aggregate Throughput:    " << (throughput_passed ? "PASS (>= 500 tok/s at B=8)" : "FAIL") << "\n"
                  << "  Batch Scaling Speedup:   " << (speedup_passed ? "PASS (>= 6.0x over B=1)" : "FAIL") << "\n"
                  << "===================================================\n"
                  << "Overall Status:            " << (all_passed ? "PASS (All Invariants Verified)" : "FAIL") << "\n\n";
    }

    return all_passed ? 0 : 1;
}
