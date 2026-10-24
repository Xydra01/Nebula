// hydrogen/include/native_decoder.hpp
// Native Autoregressive Token Decoder & Compute Graph for Project Hydrogen
// SM 8.9 (NVIDIA Ada Lovelace RTX 4070) Native TQ1_0 Inference Engine

#pragma once

#include "tensor_loader.hpp"
#include "shm_ring_buffer.hpp"
#include "tokenizer.hpp"

#include <string>
#include <vector>
#include <unordered_map>
#include <filesystem>
#include <memory>
#include <cstdint>
#include <functional>

namespace hydrogen {

struct SamplingConfig {
    float temperature = 0.7f;
    float top_p = 0.80f;
    float min_p = 0.0f;
    float repetition_penalty = 1.05f;
    uint32_t top_k = 40;
};

struct LayerTensors {
    // Norms
    const float* attn_norm = nullptr;
    const float* post_attn_norm = nullptr;

    // FFN
    const uint8_t* ffn_gate = nullptr;
    const uint8_t* ffn_up = nullptr;
    const uint8_t* ffn_down = nullptr;

    // Full Attention (if layer % 4 == 3)
    bool is_attention = false;
    const uint8_t* attn_q = nullptr;
    const uint8_t* attn_k = nullptr;
    const uint8_t* attn_v = nullptr;
    const float* attn_q_norm = nullptr;
    const float* attn_k_norm = nullptr;
    const uint8_t* attn_output = nullptr;

    // SSM (if layer % 4 != 3)
    const uint8_t* ssm_gate = nullptr;
    const uint8_t* ssm_qkv = nullptr;
    const float* ssm_conv1d = nullptr;
    const float* ssm_a = nullptr;
    const void* ssm_alpha = nullptr;
    const void* ssm_beta = nullptr;
    const float* ssm_dt = nullptr;
    const float* ssm_norm = nullptr;
    const uint8_t* ssm_out = nullptr;
};

class NativeDecoder {
public:
    NativeDecoder();
    ~NativeDecoder();

    // Initialize decoder, allocate VRAM, load weights and pre-allocate activation buffers
    bool initialize(const std::filesystem::path& model_path);

    // Autoregressively generate tokens with streaming callback
    void generate(
        const std::string& prompt,
        int max_tokens,
        const std::function<bool(uint32_t token_id, float logit, const std::string& text, bool is_final)>& on_token,
        const SamplingConfig& sampling = SamplingConfig{}
    );

    // Autoregressively generate tokens and stream to daemon over Win32 Shared Memory IPC
    void generate(
        const std::string& prompt,
        int max_tokens,
        ipc::ShmServer& server,
        uint32_t stream_id,
        const SamplingConfig& sampling = SamplingConfig{}
    );

    // Tokenizer & Detokenizer access
    Tokenizer& tokenizer() noexcept { return tokenizer_; }
    [[nodiscard]] const Tokenizer& tokenizer() const noexcept { return tokenizer_; }

    [[nodiscard]] bool is_initialized() const noexcept { return initialized_; }
    [[nodiscard]] uint32_t vocab_size() const noexcept { return vocab_size_; }
    [[nodiscard]] uint32_t layer_count() const noexcept { return layer_count_; }
    [[nodiscard]] uint32_t embedding_dim() const noexcept { return embedding_dim_; }

private:
    bool initialized_ = false;
    ModelLoader loader_;
    Tokenizer tokenizer_;

    // Model Architecture Dimensions
    uint32_t vocab_size_ = 248320;
    uint32_t layer_count_ = 64;
    uint32_t embedding_dim_ = 5120;
    uint32_t ffn_dim_ = 17408;
    uint32_t head_count_ = 24;
    uint32_t head_count_kv_ = 4;
    uint32_t head_dim_ = 256;
    uint32_t ssm_inner_ = 6144;
    float rms_norm_eps_ = 1e-6f;
    uint32_t eos_token_id_ = 248046;
    uint32_t bos_token_id_ = 248044;

    // Device Memory Pointers
    uint8_t* d_weights_ = nullptr;
    size_t total_weight_bytes_ = 0;

    // Global Model Tensors
    const uint8_t* d_token_embd_ = nullptr;
    const float* d_output_norm_ = nullptr;
    const uint8_t* d_output_ = nullptr;

    // Per-layer Tensors (64 layers)
    std::vector<LayerTensors> layers_;

    // Activation Working Buffers (Device)
    float* d_x_ = nullptr;             // [5120]
    float* d_x_norm_ = nullptr;        // [5120]
    float* d_res_ = nullptr;           // [5120]
    float* d_q_ = nullptr;             // [12288]
    float* d_k_ = nullptr;             // [1024]
    float* d_v_ = nullptr;             // [1024]
    float* d_attn_out_ = nullptr;      // [6144]
    float* d_qkv_ = nullptr;           // [10240]
    float* d_gate_ = nullptr;          // [17408]
    float* d_up_ = nullptr;            // [17408]
    float* d_act_ = nullptr;           // [17408]
    float* d_ffn_out_ = nullptr;       // [5120]
    float* d_logits_ = nullptr;        // [248320]

    // KV Cache & SSM States
    static constexpr uint32_t MAX_SEQ_LEN = 2048;
    float* d_k_cache_ = nullptr;       // [16 * MAX_SEQ_LEN * 4 * 256]
    float* d_v_cache_ = nullptr;       // [16 * MAX_SEQ_LEN * 4 * 256]
    float* d_ssm_state_ = nullptr;     // [48 * 128 * 6144]

    // Pinned Sampling Buffers (Top-K extraction)
    static constexpr size_t TOP_CANDIDATES = 128;
    uint32_t* d_top_tokens_ = nullptr;
    float* d_top_logits_ = nullptr;
    uint32_t* h_top_tokens_ = nullptr;
    float* h_top_logits_ = nullptr;

    // CUDA Stream
    void* cuda_stream_ = nullptr;

    // Internal Compute Steps
    void forward_token(uint32_t token_id, uint32_t pos, bool compute_logits);
    uint32_t sample_next_token(
        const SamplingConfig& sampling,
        const std::vector<uint32_t>& generated_tokens
    );
};

} // namespace hydrogen
