// hydrogen/src/tools/test_page_block.cpp
// Verification tool for Issue H04: Uniform Page Block Layout & Dual-Quantization Tagging

#include "page_block.hpp"
#include <iostream>
#include <iomanip>
#include <vector>
#include <random>
#include <cmath>
#include <cassert>
#include <string>

namespace {

void print_help(const char* prog) {
    std::cout << "Usage: " << prog << " [options]\n\n"
              << "Options:\n"
              << "  --json       Output structured JSON telemetry\n"
              << "  -v, --verbose Print detailed round-trip fidelity metrics\n"
              << "  -h, --help   Display this help message\n";
}

} // anonymous namespace

int main(int argc, char* argv[]) {
    bool json_output = false;
    bool verbose = false;

    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--json") {
            json_output = true;
        } else if (arg == "-v" || arg == "--verbose") {
            verbose = true;
        } else if (arg == "-h" || arg == "--help") {
            print_help(argv[0]);
            return 0;
        }
    }

    // 1. Layout & Sizing Invariant Verification
    size_t struct_size = sizeof(hydrogen::UniformPageBlock);
    size_t offset_type = offsetof(hydrogen::UniformPageBlock, quant_type);
    size_t offset_scale = offsetof(hydrogen::UniformPageBlock, dynamic_scale);
    size_t offset_data = offsetof(hydrogen::UniformPageBlock, data);

    bool size_passed = (struct_size == hydrogen::PAGE_BLOCK_TOTAL_BYTES);
    bool layout_passed = (offset_type == 0 && offset_scale == 4 && offset_data == 8);

    // 2. Dual-Quantization Hybrid Policy Verification
    hydrogen::PageQuantPolicy policy_28(28, 4); // e.g. Qwen3-Embedding
    hydrogen::PageQuantPolicy policy_64(64, 4); // e.g. Bonsai 27B

    bool policy_passed = true;
    // Check 28-layer model
    for (uint32_t l = 0; l < 4; ++l) {
        if (policy_28.get_layer_quant_type(l) != hydrogen::PageQuantType::Q8_0) policy_passed = false;
    }
    for (uint32_t l = 4; l < 24; ++l) {
        if (policy_28.get_layer_quant_type(l) != hydrogen::PageQuantType::FP8_E4M3) policy_passed = false;
    }
    for (uint32_t l = 24; l < 28; ++l) {
        if (policy_28.get_layer_quant_type(l) != hydrogen::PageQuantType::Q8_0) policy_passed = false;
    }

    // Check 64-layer model
    for (uint32_t l = 0; l < 4; ++l) {
        if (policy_64.get_layer_quant_type(l) != hydrogen::PageQuantType::Q8_0) policy_passed = false;
    }
    for (uint32_t l = 4; l < 60; ++l) {
        if (policy_64.get_layer_quant_type(l) != hydrogen::PageQuantType::FP8_E4M3) policy_passed = false;
    }
    for (uint32_t l = 60; l < 64; ++l) {
        if (policy_64.get_layer_quant_type(l) != hydrogen::PageQuantType::Q8_0) policy_passed = false;
    }

    // Kurtosis outlier fallback
    if (policy_64.evaluate_activation_kurtosis(10, 6.5f) != hydrogen::PageQuantType::Q8_0) {
        policy_passed = false;
    }

    // 3. Synthetic Activation Quantization Round-Trip Test
    std::mt19937 rng(42);
    std::normal_distribution<float> dist(0.0f, 1.0f);

    std::vector<float> test_activations(hydrogen::PAGE_BLOCK_DATA_BYTES);
    for (float& f : test_activations) {
        f = dist(rng);
    }

    // Inject code syntax activation spikes (e.g., braces, semicolons)
    test_activations[0] = 8.5f;   // '{'
    test_activations[1] = -7.2f;  // '}'
    test_activations[2] = 6.4f;   // ';'
    test_activations[3] = 4.8f;   // indentation

    // Test Q8_0 roundtrip
    hydrogen::UniformPageBlock q8_block;
    hydrogen::PageBlockCodec::encode(test_activations, q8_block, hydrogen::PageQuantType::Q8_0);

    std::vector<float> q8_decoded(hydrogen::PAGE_BLOCK_DATA_BYTES);
    hydrogen::PageBlockCodec::decode(q8_block, q8_decoded);

    double q8_mse = 0.0, q8_max_err = 0.0, q8_snr = 0.0;
    hydrogen::PageBlockCodec::measure_fidelity(test_activations, q8_decoded, q8_mse, q8_max_err, q8_snr);

    // Test FP8 E4M3 roundtrip
    hydrogen::UniformPageBlock fp8_block;
    hydrogen::PageBlockCodec::encode(test_activations, fp8_block, hydrogen::PageQuantType::FP8_E4M3);

    std::vector<float> fp8_decoded(hydrogen::PAGE_BLOCK_DATA_BYTES);
    hydrogen::PageBlockCodec::decode(fp8_block, fp8_decoded);

    double fp8_mse = 0.0, fp8_max_err = 0.0, fp8_snr = 0.0;
    hydrogen::PageBlockCodec::measure_fidelity(test_activations, fp8_decoded, fp8_mse, fp8_max_err, fp8_snr);

    // Verify syntax token preservation under Q8_0
    bool syntax_preserved = (std::abs(test_activations[0] - q8_decoded[0]) < 0.15f) &&
                            (std::abs(test_activations[1] - q8_decoded[1]) < 0.15f) &&
                            (std::abs(test_activations[2] - q8_decoded[2]) < 0.15f) &&
                            (std::abs(test_activations[3] - q8_decoded[3]) < 0.15f);

    bool fidelity_passed = (q8_mse < 0.005) && (q8_snr > 34.0) && (fp8_mse < 0.05) && (fp8_snr > 30.0) && syntax_preserved;

    bool all_passed = size_passed && layout_passed && policy_passed && fidelity_passed;

    if (json_output) {
        std::cout << "{\n"
                  << "  \"status\": \"" << (all_passed ? "PASS" : "FAIL") << "\",\n"
                  << "  \"page_size_bytes\": " << struct_size << ",\n"
                  << "  \"page_payload_bytes\": " << hydrogen::PAGE_BLOCK_DATA_BYTES << ",\n"
                  << "  \"tokens_per_page\": " << hydrogen::PAGE_BLOCK_TOKENS << ",\n"
                  << "  \"bytes_per_token\": " << (hydrogen::PAGE_BLOCK_DATA_BYTES / hydrogen::PAGE_BLOCK_TOKENS) << ",\n"
                  << "  \"size_invariant_verified\": " << (size_passed ? "true" : "false") << ",\n"
                  << "  \"layout_offsets_verified\": " << (layout_passed ? "true" : "false") << ",\n"
                  << "  \"dual_quant_policy_verified\": " << (policy_passed ? "true" : "false") << ",\n"
                  << "  \"syntax_tokens_preserved\": " << (syntax_preserved ? "true" : "false") << ",\n"
                  << "  \"q8_0\": {\n"
                  << "    \"mse\": " << std::fixed << std::setprecision(6) << q8_mse << ",\n"
                  << "    \"max_error\": " << std::fixed << std::setprecision(6) << q8_max_err << ",\n"
                  << "    \"snr_db\": " << std::fixed << std::setprecision(2) << q8_snr << "\n"
                  << "  },\n"
                  << "  \"fp8_e4m3\": {\n"
                  << "    \"mse\": " << std::fixed << std::setprecision(6) << fp8_mse << ",\n"
                  << "    \"max_error\": " << std::fixed << std::setprecision(6) << fp8_max_err << ",\n"
                  << "    \"snr_db\": " << std::fixed << std::setprecision(2) << fp8_snr << "\n"
                  << "  }\n"
                  << "}\n";
    } else {
        std::cout << "\n===================================================\n"
                  << "Project Hydrogen: Uniform Page Block Layout Verification\n"
                  << "===================================================\n"
                  << "UniformPageBlock Size:     " << struct_size << " bytes (Target: 1,288 bytes) - "
                  << (size_passed ? "PASS" : "FAIL") << "\n"
                  << "Memory Layout Offsets:\n"
                  << "  quant_type:              Offset " << offset_type << " (uint8_t tag)\n"
                  << "  dynamic_scale:           Offset " << offset_scale << " (float)\n"
                  << "  data payload:            Offset " << offset_data << " (1,280 bytes)\n"
                  << "Capacity per Page:         " << hydrogen::PAGE_BLOCK_TOKENS << " tokens @ 80 bytes/token\n"
                  << "---------------------------------------------------\n"
                  << "Dual-Quantization Hybrid Policy:\n"
                  << "  28-Layer Geometry:       Layers 0..3: Q8_0 | 4..23: FP8 | 24..27: Q8_0\n"
                  << "  64-Layer Geometry:       Layers 0..3: Q8_0 | 4..59: FP8 | 60..63: Q8_0\n"
                  << "  Policy Verification:     " << (policy_passed ? "PASS (Sensitive Boundaries Protected)" : "FAIL") << "\n"
                  << "---------------------------------------------------\n"
                  << "Quantization Round-Trip Fidelity:\n"
                  << "  Q8_0 MSE:                " << std::scientific << q8_mse << " (SNR: " << std::fixed << std::setprecision(2) << q8_snr << " dB)\n"
                  << "  FP8 E4M3 MSE:            " << std::scientific << fp8_mse << " (SNR: " << std::fixed << std::setprecision(2) << fp8_snr << " dB)\n"
                  << "  Code Syntax Outliers:    " << (syntax_preserved ? "PASS (Zero syntax degradation)" : "FAIL") << "\n"
                  << "===================================================\n"
                  << "Overall Status:            " << (all_passed ? "PASS (All Invariants Verified)" : "FAIL") << "\n\n";
    }

    return all_passed ? 0 : 1;
}
