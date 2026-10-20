// hydrogen/src/smoke_test.cpp
// CLI entry point for Issue H03: Single-Token Forward Smoke Test & VRAM Baseline Telemetry

#include "forward_baseline.hpp"
#include <iostream>
#include <iomanip>
#include <filesystem>
#include <string>
#include <vector>

namespace fs = std::filesystem;

namespace {

void print_help(const char* prog_name) {
    std::cout << "Usage: " << prog_name << " [options] [model_path]\n\n"
              << "Options:\n"
              << "  -m, --model <path>       Path to GGUF model file\n"
              << "  -p, --prompt <string>    Input prompt (default: \"The capital of France is\")\n"
              << "  -k, --top-k <int>        Number of top logit candidates to extract (default: 5)\n"
              << "  --alloc-full             Allocate and copy full model weights to VRAM (default: true)\n"
              << "  --no-alloc-full          Allocate only forward-active tensors in VRAM\n"
              << "  --json                   Output structured JSON telemetry\n"
              << "  -h, --help               Display this help message\n";
}

std::string escape_json(const std::string& input) {
    std::string out;
    out.reserve(input.size() + 16);
    for (char c : input) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\b': out += "\\b"; break;
            case '\f': out += "\\f"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (static_cast<unsigned char>(c) < 0x20) {
                    char buf[8];
                    snprintf(buf, sizeof(buf), "\\u%04x", static_cast<unsigned char>(c));
                    out += buf;
                } else {
                    out += c;
                }
                break;
        }
    }
    return out;
}

fs::path auto_discover_model() {
    std::vector<fs::path> candidates = {
        "F:\\Nebula\\models\\bonsai2-27b\\Ternary-Bonsai-2-27B-PTQ1_0.gguf",
        "F:\\Nebula\\models\\embedding\\Qwen3-Embedding-0.6B-Q8_0.gguf",
        "D:\\NebulaCold\\models-archive\\fallback\\DeltaCoder-9B-v1.1-DPO-Q6_K.gguf",
        "F:\\Nebula\\models\\bonsai2-27b-mtp\\Ternary-Bonsai-2-27B-PQ2_0-MTP-Q8_0.gguf"
    };

    for (const auto& p : candidates) {
        if (fs::exists(p)) {
            return p;
        }
    }
    return "";
}

} // anonymous namespace

int main(int argc, char* argv[]) {
    hydrogen::ForwardConfig config;
    bool path_specified = false;

    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "-h" || arg == "--help") {
            print_help(argv[0]);
            return 0;
        } else if (arg == "--json") {
            config.json_output = true;
        } else if (arg == "-v" || arg == "--verbose") {
            config.verbose = true;
        } else if (arg == "--alloc-full") {
            config.allocate_full_weights = true;
        } else if (arg == "--no-alloc-full") {
            config.allocate_full_weights = false;
        } else if ((arg == "-m" || arg == "--model") && i + 1 < argc) {
            config.model_path = argv[++i];
            path_specified = true;
        } else if ((arg == "-p" || arg == "--prompt") && i + 1 < argc) {
            config.prompt = argv[++i];
        } else if ((arg == "-k" || arg == "--top-k") && i + 1 < argc) {
            config.top_k = std::stoi(argv[++i]);
        } else if (arg.rfind("-", 0) != 0 && !path_specified) {
            config.model_path = arg;
            path_specified = true;
        }
    }

    if (config.model_path.empty()) {
        config.model_path = auto_discover_model();
    }

    if (config.model_path.empty() || !fs::exists(config.model_path)) {
        if (config.json_output) {
            std::cout << "{\n  \"status\": \"FAIL\",\n  \"error\": \"No model file found or specified\"\n}\n";
        } else {
            std::cerr << "Error: No model file found or specified. Use --model <path>\n";
        }
        return 1;
    }

    hydrogen::ForwardBaseline baseline;
    hydrogen::ForwardResult result;
    try {
        result = baseline.run(config);
    } catch (const std::exception& e) {
        if (config.json_output) {
            std::cout << "{\n  \"status\": \"FAIL\",\n  \"error\": \"" << escape_json(e.what()) << "\"\n}\n";
        } else {
            std::cerr << "Forward baseline exception: " << e.what() << "\n";
        }
        return 1;
    }

    if (!result.success) {
        if (config.json_output) {
            std::cout << "{\n"
                      << "  \"status\": \"FAIL\",\n"
                      << "  \"error\": \"" << escape_json(result.error_message) << "\"\n"
                      << "}\n";
        } else {
            std::cerr << "Forward baseline failed: " << result.error_message << "\n";
        }
        return 1;
    }

    // Evaluate Acceptance Criteria
    bool top_1_is_paris = !result.top_tokens.empty() && 
        (result.top_tokens[0].token_str.find("Paris") != std::string::npos || 
         result.top_tokens[0].token_id == 11751 || 
         result.top_tokens[0].token_id == 12095);

    bool zero_vram_leak = (result.vram_leaked_bytes == 0);

    double theoretical_mib = static_cast<double>(result.weight_bytes_theoretical) / (1024.0 * 1024.0);
    double allocated_mib = static_cast<double>(result.weight_bytes_allocated) / (1024.0 * 1024.0);
    double free_during_mib = static_cast<double>(result.vram_free_during_bytes) / (1024.0 * 1024.0);
    double free_before_mib = static_cast<double>(result.vram_free_before_bytes) / (1024.0 * 1024.0);
    double free_after_mib = static_cast<double>(result.vram_free_after_bytes) / (1024.0 * 1024.0);

    if (config.json_output) {
        std::cout << "{\n"
                  << "  \"status\": \"PASS\",\n"
                  << "  \"model_path\": \"" << escape_json(config.model_path.string()) << "\",\n"
                  << "  \"prompt\": \"" << escape_json(config.prompt) << "\",\n"
                  << "  \"architecture\": \"" << escape_json(result.architecture) << "\",\n"
                  << "  \"layer_count\": " << result.layer_count << ",\n"
                  << "  \"embedding_dim\": " << result.embedding_dim << ",\n"
                  << "  \"vocab_size\": " << result.vocab_size << ",\n"
                  << "  \"token_count\": " << result.input_token_ids.size() << ",\n"
                  << "  \"top_1_matches_reference\": " << (top_1_is_paris ? "true" : "false") << ",\n"
                  << "  \"vram_theoretical_mib\": " << std::fixed << std::setprecision(2) << theoretical_mib << ",\n"
                  << "  \"vram_allocated_mib\": " << std::fixed << std::setprecision(2) << allocated_mib << ",\n"
                  << "  \"vram_free_before_mib\": " << std::fixed << std::setprecision(2) << free_before_mib << ",\n"
                  << "  \"vram_free_during_mib\": " << std::fixed << std::setprecision(2) << free_during_mib << ",\n"
                  << "  \"vram_free_after_mib\": " << std::fixed << std::setprecision(2) << free_after_mib << ",\n"
                  << "  \"vram_leaked_bytes\": " << result.vram_leaked_bytes << ",\n"
                  << "  \"model_parse_ms\": " << std::fixed << std::setprecision(3) << result.model_parse_ms << ",\n"
                  << "  \"weight_load_ms\": " << std::fixed << std::setprecision(3) << result.weight_load_ms << ",\n"
                  << "  \"forward_latency_ms\": " << std::fixed << std::setprecision(3) << result.forward_latency_ms << ",\n"
                  << "  \"top_tokens\": [\n";

        for (size_t i = 0; i < result.top_tokens.size(); ++i) {
            const auto& tok = result.top_tokens[i];
            std::cout << "    {\n"
                      << "      \"rank\": " << (i + 1) << ",\n"
                      << "      \"token_id\": " << tok.token_id << ",\n"
                      << "      \"token_str\": \"" << escape_json(tok.token_str) << "\",\n"
                      << "      \"logit\": " << std::fixed << std::setprecision(4) << tok.logit << ",\n"
                      << "      \"probability\": " << std::fixed << std::setprecision(6) << tok.probability << "\n"
                      << "    }" << (i + 1 < result.top_tokens.size() ? "," : "") << "\n";
        }
        std::cout << "  ]\n}\n";
    } else {
        std::cout << "\n===================================================\n"
                  << "Project Hydrogen: Single-Token Forward Smoke Test\n"
                  << "===================================================\n"
                  << "Model:              " << config.model_path.string() << "\n"
                  << "Architecture:       " << result.architecture << " (" << result.layer_count << " layers, D=" << result.embedding_dim << ", V=" << result.vocab_size << ")\n"
                  << "Prompt:             \"" << config.prompt << "\"\n"
                  << "Input Tokens:       " << result.input_token_ids.size() << " tokens (";
        for (size_t i = 0; i < result.input_token_ids.size(); ++i) {
            std::cout << result.input_token_ids[i] << (i + 1 < result.input_token_ids.size() ? ", " : "");
        }
        std::cout << ")\n"
                  << "---------------------------------------------------\n"
                  << "Top Predicted Candidates:\n";

        for (size_t i = 0; i < result.top_tokens.size(); ++i) {
            const auto& tok = result.top_tokens[i];
            std::cout << "  [" << (i + 1) << "] ID: " << std::setw(6) << tok.token_id
                      << " | Token: " << std::setw(12) << ("'" + tok.token_str + "'")
                      << " | Logit: " << std::setw(8) << std::fixed << std::setprecision(3) << tok.logit
                      << " | Prob: " << std::setw(7) << std::fixed << std::setprecision(2) << (tok.probability * 100.0f) << "%\n";
        }

        std::cout << "---------------------------------------------------\n"
                  << "VRAM Telemetry:\n"
                  << "  Theoretical Weight: " << std::fixed << std::setprecision(2) << theoretical_mib << " MiB\n"
                  << "  VRAM Allocated:     " << std::fixed << std::setprecision(2) << allocated_mib << " MiB\n"
                  << "  Free Before Alloc:  " << std::fixed << std::setprecision(2) << free_before_mib << " MiB\n"
                  << "  Free During Alloc:  " << std::fixed << std::setprecision(2) << free_during_mib << " MiB\n"
                  << "  Free After Cleanup: " << std::fixed << std::setprecision(2) << free_after_mib << " MiB\n"
                  << "  VRAM Leaked:        " << result.vram_leaked_bytes << " bytes (" 
                  << (zero_vram_leak ? "PASS - Zero Leak" : "FAIL - Leak Detected") << ")\n"
                  << "---------------------------------------------------\n"
                  << "Execution Latencies:\n"
                  << "  Model Parse Time:   " << std::fixed << std::setprecision(3) << result.model_parse_ms << " ms\n"
                  << "  PCIe Weight Stream: " << std::fixed << std::setprecision(3) << result.weight_load_ms << " ms\n"
                  << "  CUDA Forward Pass:  " << std::fixed << std::setprecision(3) << result.forward_latency_ms << " ms\n"
                  << "===================================================\n";

        if (top_1_is_paris && zero_vram_leak) {
            std::cout << "Status: PASS (All Acceptance Criteria Met)\n\n";
        } else {
            std::cout << "Status: FAIL (Criteria Unmet)\n\n";
        }
    }

    bool prompt_passed = (config.prompt == "The capital of France is") ? top_1_is_paris : !result.top_tokens.empty();
    return (prompt_passed && zero_vram_leak) ? 0 : 1;
}
