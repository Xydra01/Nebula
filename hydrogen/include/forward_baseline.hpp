#pragma once

#include "gguf_parser.hpp"
#include "tensor_loader.hpp"
#include <string>
#include <vector>
#include <cstdint>
#include <cstddef>
#include <filesystem>

namespace hydrogen {

/**
 * @brief Configuration parameters for forward smoke test execution.
 */
struct ForwardConfig {
    std::filesystem::path model_path;
    std::string prompt = "The capital of France is";
    int top_k = 5;
    bool allocate_full_weights = true;
    bool verbose = false;
    bool json_output = false;
};

/**
 * @brief Logit candidate with token ID, string representation, logit, and probability.
 */
struct TokenLogit {
    uint32_t token_id = 0;
    std::string token_str;
    float logit = 0.0f;
    float probability = 0.0f;
};

/**
 * @brief Telemetry and verification metrics from forward baseline execution.
 */
struct ForwardResult {
    bool success = false;
    std::string error_message;
    std::string architecture;
    uint32_t layer_count = 0;
    uint32_t embedding_dim = 0;
    uint32_t vocab_size = 0;
    
    std::vector<uint32_t> input_token_ids;
    std::vector<TokenLogit> top_tokens;
    
    // Memory telemetry (in bytes)
    size_t weight_bytes_theoretical = 0;
    size_t weight_bytes_allocated = 0;
    size_t vram_free_before_bytes = 0;
    size_t vram_free_during_bytes = 0;
    size_t vram_free_after_bytes = 0;
    size_t vram_leaked_bytes = 0;
    
    // Performance telemetry (in milliseconds)
    float model_parse_ms = 0.0f;
    float weight_load_ms = 0.0f;
    float forward_latency_ms = 0.0f;
};

/**
 * @brief CUDA SM 8.9 baseline forward pass executor and VRAM profiler.
 */
class ForwardBaseline {
public:
    ForwardBaseline() = default;
    ~ForwardBaseline();

    // Non-copyable, movable
    ForwardBaseline(const ForwardBaseline&) = delete;
    ForwardBaseline& operator=(const ForwardBaseline&) = delete;
    ForwardBaseline(ForwardBaseline&&) noexcept = default;
    ForwardBaseline& operator=(ForwardBaseline&&) noexcept = default;

    /**
     * @brief Run baseline forward pass and VRAM profiling.
     */
    ForwardResult run(const ForwardConfig& config);

    /**
     * @brief Simple subword/BPE tokenization against GGUF vocabulary metadata.
     */
    static std::vector<uint32_t> tokenize(const GGUFModel& model, const std::string& prompt);

    /**
     * @brief Look up human-readable token string for a token ID.
     */
    static std::string get_token_str(const GGUFModel& model, uint32_t token_id);
};

} // namespace hydrogen
