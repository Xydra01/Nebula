#pragma once

#include <cstdint>
#include <cstddef>
#include <string>
#include <vector>
#include <unordered_map>
#include <memory>
#include <cuda_runtime.h>

namespace hydrogen::grammar {

// Architecture & Vocabulary Constants (SM 8.9 / Qwen 2.5 Bonsai)
constexpr uint32_t VOCAB_SIZE = 152064;
constexpr uint32_t MASK_WORDS = (VOCAB_SIZE + 31) / 32; // Exactly 4,752 uint32_t words
constexpr size_t MASK_SIZE_BYTES = MASK_WORDS * sizeof(uint32_t); // Exactly 19,008 bytes (~19.0 KiB)

/**
 * @brief Supported JSON schema property types for tool argument constraints.
 */
enum class PropertyType {
    String,
    Integer,
    Boolean,
    StringArray
};

/**
 * @brief Schema definition for an individual tool parameter.
 */
struct PropertySchema {
    std::string name;
    PropertyType type = PropertyType::String;
    bool required = true;
};

/**
 * @brief Schema definition for a registered tool function.
 */
struct ToolSchema {
    std::string name;
    std::vector<PropertySchema> properties;
};

/**
 * @brief Explicit state transition entry for GPU device execution.
 */
struct FsmTransition {
    uint32_t token_id;
    int next_state;
};

/**
 * @brief Description of an FSM state formatted for direct GPU SRAM and global execution.
 */
struct FsmStateDesc {
    uint32_t mask_offset_words = 0;
    int default_next_state = -1;
    uint32_t transition_offset = 0;
    uint32_t transition_count = 0;
    bool is_terminal = false;
    char name[48] = {0};
};

/**
 * @brief Pre-compiled deterministic FSM table with bitmasks and transitions.
 */
struct FsmCompiledTable {
    uint32_t vocab_size = VOCAB_SIZE;
    uint32_t mask_words = MASK_WORDS;
    int start_state = 0;

    // Concatenated bitmask table: [num_states * MASK_WORDS]
    std::vector<uint32_t> bitmasks;

    // State descriptors
    std::vector<FsmStateDesc> states;

    // Transitions table
    std::vector<FsmTransition> transitions;

    // Human-readable vocabulary mapping for simulation & decoding
    std::unordered_map<uint32_t, std::string> token_to_str;
    std::unordered_map<std::string, uint32_t> str_to_token;

    [[nodiscard]] size_t num_states() const { return states.size(); }
    [[nodiscard]] size_t total_mask_bytes() const { return bitmasks.size() * sizeof(uint32_t); }
    [[nodiscard]] bool is_token_allowed(int state_id, uint32_t token_id) const;
    [[nodiscard]] int get_next_state(int state_id, uint32_t token_id) const;
};

/**
 * @brief Compiler that transforms registered JSON tool schemas into deterministic FSM tables.
 */
class FsmCompiler {
public:
    FsmCompiler();

    /**
     * @brief Register a tool schema for FSM pre-compilation.
     */
    void register_tool(const ToolSchema& schema);

    /**
     * @brief Pre-compile all registered tools into a unified deterministic FSM table.
     */
    [[nodiscard]] FsmCompiledTable compile();

private:
    std::vector<ToolSchema> schemas_;
    void initialize_vocabulary(FsmCompiledTable& table);
};

/**
 * @brief GPU memory buffers for zero-synchronization FSM bitmask sampling.
 */
struct FsmGpuBuffers {
    uint32_t* d_bitmasks = nullptr;
    FsmStateDesc* d_states = nullptr;
    FsmTransition* d_transitions = nullptr;
    int* d_state = nullptr;
    uint32_t* d_sampled_token = nullptr;
    float* d_logits = nullptr;

    // Host pinned pointers for asynchronous non-blocking inspection
    int* h_pinned_state = nullptr;
    uint32_t* h_pinned_token = nullptr;

    size_t allocated_states = 0;
    size_t allocated_transitions = 0;

    void allocate(const FsmCompiledTable& table);
    void free();
    void reset(int start_state = 0);
};

/**
 * @brief Detailed performance metrics comparing GPU FSM sampling against CPU GBNF baseline.
 */
struct FsmBenchmarkMetrics {
    float gpu_validation_latency_us = 0.0f;
    float cpu_gbnf_latency_us = 0.0f;
    float speedup_factor = 0.0f;
    uint32_t stream_sync_count = 0;
    uint32_t total_tool_calls_generated = 0;
    uint32_t json_syntax_errors = 0;
    float schema_conformity_pct = 0.0f;
    bool sram_staging_verified = false;
    size_t sram_footprint_bytes = 0;
    size_t active_states_count = 0;
};

/**
 * @brief GPU-fused FSM bitmask sampler with on-chip SRAM staging.
 */
class FsmSampler {
public:
    /**
     * @brief Stage active state bitmask into Shared Memory (SRAM) and apply in-place logit masking.
     *
     * @param d_logits Device pointer to forward logits [vocab_size].
     * @param vocab_size Vocabulary size (152,064).
     * @param d_bitmasks Device pointer to concatenated bitmask table.
     * @param d_state Device pointer to active state index.
     * @param stream CUDA stream.
     */
    static void apply_bitmask(
        float* d_logits,
        uint32_t vocab_size,
        const uint32_t* d_bitmasks,
        const int* d_state,
        cudaStream_t stream = nullptr
    );

    /**
     * @brief Sample token on GPU and write to d_sampled_token.
     */
    static void sample_token(
        const float* d_logits,
        uint32_t vocab_size,
        uint32_t* d_sampled_token,
        float temperature,
        cudaStream_t stream = nullptr
    );

    /**
     * @brief Execute FSM state transition directly on GPU without CPU round-trips.
     */
    static void execute_transition(
        const uint32_t* d_sampled_token,
        int* d_state,
        const FsmStateDesc* d_states,
        const FsmTransition* d_transitions,
        cudaStream_t stream = nullptr
    );

    /**
     * @brief Fused kernel dispatch executing bitmask application, sampling, and state transition.
     */
    static void fused_step(
        float* d_logits,
        uint32_t vocab_size,
        const uint32_t* d_bitmasks,
        int* d_state,
        uint32_t* d_sampled_token,
        const FsmStateDesc* d_states,
        const FsmTransition* d_transitions,
        float temperature,
        cudaStream_t stream = nullptr
    );

    /**
     * @brief Run end-to-end benchmark verifying latency, 0 CPU syncs, and 10,000 JSON tool calls.
     */
    static FsmBenchmarkMetrics run_benchmark(
        const FsmCompiledTable& table,
        uint32_t num_tool_calls = 10000,
        uint32_t latency_iters = 1000
    );
};

} // namespace hydrogen::grammar
