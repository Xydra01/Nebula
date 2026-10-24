// hydrogen/src/tools/test_tokenizer.cpp
// Standalone Unit Test Binary for Project Hydrogen Tokenizer (Issue H19)

#include "tokenizer.hpp"
#include "tensor_loader.hpp"
#include <iostream>
#include <iomanip>
#include <string>
#include <vector>
#include <cassert>
#include <filesystem>

namespace fs = std::filesystem;

namespace {

struct TestCase {
    std::string name;
    std::string input;
    bool contains_special;
};

fs::path auto_discover_model() {
    std::vector<fs::path> candidates = {
        "F:\\Nebula\\models\\bonsai2-27b\\Ternary-Bonsai-2-27B-PTQ1_0.gguf",
        "F:\\Nebula\\models\\bonsai2-27b-mtp\\Ternary-Bonsai-2-27B-PQ2_0-MTP-Q8_0.gguf",
        "D:\\NebulaCold\\models-archive\\fallback\\DeltaCoder-9B-v1.1-DPO-Q6_K.gguf",
        "F:\\Nebula\\models\\embedding\\Qwen3-Embedding-0.6B-Q8_0.gguf"
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
    std::string model_path_str;
    bool json_output = false;

    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--json") {
            json_output = true;
        } else if (arg == "-m" || arg == "--model") {
            if (i + 1 < argc) model_path_str = argv[++i];
        } else if (!arg.empty() && arg[0] != '-') {
            model_path_str = arg;
        }
    }

    fs::path model_path = model_path_str.empty() ? auto_discover_model() : fs::path(model_path_str);

    if (!fs::exists(model_path)) {
        std::cerr << "Model file not found: " << model_path << "\n";
        return 1;
    }

    if (!json_output) {
        std::cout << "========================================================\n"
                  << " Project Hydrogen: Standalone Tokenizer Unit Test (H19)\n"
                  << "========================================================\n"
                  << " Model: " << model_path.string() << "\n"
                  << "========================================================\n";
    }

    hydrogen::ModelLoader loader;
    try {
        loader.load(model_path);
    } catch (const std::exception& e) {
        std::cerr << "Failed to load GGUF model: " << e.what() << "\n";
        return 1;
    }

    hydrogen::Tokenizer tokenizer;
    if (!tokenizer.load(loader.model())) {
        std::cerr << "Failed to load tokenizer metadata\n";
        return 1;
    }

    // 1. Vocabulary metrics
    uint32_t vocab_size = tokenizer.vocab_size();
    bool is_bpe = tokenizer.is_bpe();

    // 2. Test vectors
    std::vector<TestCase> test_cases = {
        {"Basic English", "Hello, world! Welcome to Project Hydrogen.", false},
        {"Rust Syntax", "fn main() {\n    println!(\"Hello from Nebula!\");\n}", false},
        {"Punctuation and Whitespace", "  Spaces, tabs:\t\nand newlines... 1234567890!@#$%^&*()", false},
        {"ChatML Special Tokens", "<|im_start|>user\nWhat is 2 + 2?<|im_end|>\n<|im_start|>assistant\n", true},
        {"Tool Call Tags", "<tool_call>\n{\"name\": \"fs.read\", \"arguments\": {\"path\": \"src/lib.rs\"}}\n</tool_call>", true},
        {"UTF-8 Byte Fallback", "Greek: αβγ, Emoji: 🚀🔥, Math: ∑∫dx = ∞", false}
    };

    size_t passed_count = 0;
    size_t total_tokens_encoded = 0;

    for (const auto& tc : test_cases) {
        std::vector<uint32_t> encoded = tokenizer.encode(tc.input);
        total_tokens_encoded += encoded.size();
        std::string decoded = tokenizer.decode(encoded);

        bool success = !encoded.empty();

        // Verify special tokens are kept as atomic tokens
        if (tc.name == "ChatML Special Tokens") {
            bool has_im_start = false;
            bool has_im_end = false;
            for (uint32_t tid : encoded) {
                if (tid == tokenizer.im_start_id()) has_im_start = true;
                if (tid == tokenizer.im_end_id()) has_im_end = true;
            }
            if (!has_im_start || !has_im_end) {
                success = false;
            }
        }

        if (success) {
            passed_count++;
            if (!json_output) {
                std::cout << " [PASS] " << std::left << std::setw(30) << tc.name
                          << " | " << encoded.size() << " tokens\n";
            }
        } else {
            if (!json_output) {
                std::cout << " [FAIL] " << std::left << std::setw(30) << tc.name << "\n";
            }
        }
    }

    // 3. Test ChatML conversation formatting
    std::vector<hydrogen::ChatMessageInput> messages = {
        {"system", "You are Nebula, a helpful assistant."},
        {"user", "Write a fibonacci function in Rust."}
    };

    std::string formatted = tokenizer.format_chatml(messages, true);
    std::vector<uint32_t> formatted_tokens = tokenizer.encode(formatted);

    bool chatml_ok = (formatted_tokens.size() > 10) &&
                     (formatted_tokens[0] == tokenizer.im_start_id());

    if (chatml_ok) passed_count++;

    bool all_passed = (passed_count == test_cases.size() + 1);

    if (json_output) {
        std::cout << "{\n"
                  << "  \"benchmark\": \"H19_standalone_tokenizer\",\n"
                  << "  \"status\": \"" << (all_passed ? "PASS" : "FAIL") << "\",\n"
                  << "  \"vocab_size\": " << vocab_size << ",\n"
                  << "  \"is_bpe\": " << (is_bpe ? "true" : "false") << ",\n"
                  << "  \"im_start_token_id\": " << tokenizer.im_start_id() << ",\n"
                  << "  \"im_end_token_id\": " << tokenizer.im_end_id() << ",\n"
                  << "  \"total_tokens_encoded\": " << total_tokens_encoded << ",\n"
                  << "  \"test_cases_passed\": " << passed_count << ",\n"
                  << "  \"test_cases_total\": " << (test_cases.size() + 1) << ",\n"
                  << "  \"all_criteria_met\": " << (all_passed ? "true" : "false") << "\n"
                  << "}\n";
    } else {
        std::cout << "--------------------------------------------------------\n"
                  << " Vocab Size: " << vocab_size << " tokens\n"
                  << " Tokenizer Type: " << (is_bpe ? "BPE (merges table present)" : "SentencePiece / Unigram") << "\n"
                  << " Tests Passed: " << passed_count << " / " << (test_cases.size() + 1) << "\n"
                  << " Status: " << (all_passed ? "ALL TESTS PASSED" : "FAILED") << "\n"
                  << "========================================================\n";
    }

    return all_passed ? 0 : 1;
}
