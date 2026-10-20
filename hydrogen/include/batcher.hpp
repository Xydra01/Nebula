// hydrogen/include/batcher.hpp
// Split-Fuse Continuous Batching Scheduler with 64-token prefill chunking (Issue H10)

#pragma once

#include <cstdint>
#include <cstddef>
#include <vector>
#include <deque>
#include <memory>
#include <string>
#include <span>
#include <chrono>
#include "page_block.hpp"

namespace hydrogen {

/**
 * @brief Lifecycle status of a sequence request in the Split-Fuse scheduler.
 */
enum class RequestStatus : uint8_t {
    QUEUED = 0,
    RUNNING_PREFILL = 1,
    RUNNING_DECODE = 2,
    COMPLETED = 3
};

/**
 * @brief Sequence request descriptor tracking prefill chunking and decode generation progress.
 */
struct SequenceRequest {
    uint32_t request_id = 0;
    uint32_t prompt_tokens = 0;
    uint32_t prefill_completed_tokens = 0;
    uint32_t decode_tokens_generated = 0;
    uint32_t max_decode_tokens = 0;
    RequestStatus status = RequestStatus::QUEUED;

    double arrival_time_ms = 0.0;
    double prefill_finish_time_ms = 0.0;
    double completion_time_ms = 0.0;
};

/**
 * @brief Slice of tokens from a request assigned to a continuous forward step.
 */
struct BatchChunk {
    uint32_t request_id = 0;
    uint32_t start_token = 0;
    uint32_t num_tokens = 0;
    bool is_prefill = false; // true if chunking prefill prompt, false if 1 decode token
};

/**
 * @brief Fused continuous batch combining active decode requests and prefill chunks.
 */
struct FusedBatch {
    std::vector<BatchChunk> chunks;
    uint32_t num_decode_requests = 0;
    uint32_t num_prefill_tokens = 0;
    uint32_t total_batch_tokens = 0;
    double assembly_overhead_us = 0.0; // Invariant: < 15.0 us
};

/**
 * @brief Configuration parameters for Split-Fuse continuous batching.
 */
struct SplitFuseConfig {
    uint32_t max_prefill_chunk_tokens = 64;   // Strictly clamped to 64 tokens
    uint32_t max_decode_streams = 16;         // Up to 16 concurrent decode streams
    uint32_t max_batch_tokens = 512;          // Maximum tokens per fused forward step
    float target_decode_step_ms = 14.08f;     // Decode memory read time (RTX 4070 504 GB/s)
    float max_allowed_decode_jitter_pct = 10.0f; // Decode latency jitter bound (< 10%)
    float max_decode_step_latency_ms = 15.5f; // Hard ceiling on decode step latency
    size_t preallocated_workspace_bytes = 64 * 1024 * 1024; // 64 MB pre-allocated workspace
};

/**
 * @brief Benchmark telemetry results for Split-Fuse continuous batching verification.
 */
struct SplitFuseBenchmarkResult {
    double avg_assembly_overhead_us = 0.0;
    double max_assembly_overhead_us = 0.0;
    bool assembly_overhead_passed = true; // Invariant: < 15.0 us

    float decode_latency_before_prefill_ms = 0.0f;
    float decode_latency_during_prefill_ms = 0.0f;
    float decode_latency_after_prefill_ms = 0.0f;
    float decode_jitter_pct = 0.0f;
    bool decode_jitter_passed = true; // Invariant: <= 10.0%

    float max_decode_step_latency_ms = 0.0f;
    bool max_decode_latency_passed = true; // Invariant: <= 15.5 ms

    uint32_t prefill_iterations = 0; // 1500 tokens / 64 = 24 iterations
    float prefill_total_duration_ms = 0.0f;
    bool prefill_duration_passed = true; // Invariant: < 300 ms

    uint32_t max_prefill_chunk_observed = 0;
    bool max_chunk_clamped_passed = true; // Invariant: <= 64 tokens

    bool zero_stutter_passed = true;
    bool all_invariants_met = true;
};

/**
 * @brief Split-Fuse Continuous Batching Scheduler.
 *
 * Partitions incoming massive tool observation prefills into 64-token micro-chunks
 * and fuses them with active token decoding streams, eliminating UI stutter and
 * keeping per-token decode latency strictly under 15.5 ms (<10% jitter).
 */
class SplitFuseBatcher {
public:
    explicit SplitFuseBatcher(const SplitFuseConfig& config = SplitFuseConfig{});
    ~SplitFuseBatcher();

    // Non-copyable, non-movable
    SplitFuseBatcher(const SplitFuseBatcher&) = delete;
    SplitFuseBatcher& operator=(const SplitFuseBatcher&) = delete;
    SplitFuseBatcher(SplitFuseBatcher&&) = delete;
    SplitFuseBatcher& operator=(SplitFuseBatcher&&) = delete;

    /**
     * @brief Submit a new request to the scheduler.
     * @param prompt_tokens Length of the prompt or tool observation in tokens.
     * @param max_decode_tokens Maximum number of tokens to generate in decode phase.
     * @return Generated unique request ID.
     */
    uint32_t submit_request(uint32_t prompt_tokens, uint32_t max_decode_tokens);

    /**
     * @brief Assemble the next continuous forward step batch.
     * Combines active decode streams with at most 64 tokens from the prefill queue.
     * Guarantees assembly overhead < 15.0 us.
     */
    FusedBatch assemble_step_batch();

    /**
     * @brief Advance request state after forward step execution.
     * Updates completed prefill token counts and generated decode token counts.
     */
    void update_step_completion(const FusedBatch& batch);

    /**
     * @brief Execute a fused step using pre-allocated GPU workspace.
     * Measures exact hardware execution time via CUDA events.
     * @return Execution latency in ms.
     */
    float execute_step(const FusedBatch& batch, void* stream = nullptr);

    /**
     * @brief Run end-to-end simulation test suite verifying Issue H10 invariants:
     * - 8 active decode streams running continuous token generation.
     * - Injects 1,500-token prefill on stream 9.
     * - Measures decode step latency before, during, and after prefill.
     * - Verifies step latency <= 15.5 ms, jitter < 10%, prefill completes in < 300 ms across 24 iterations.
     */
    SplitFuseBenchmarkResult run_simulation(
        uint32_t num_decode_streams = 8,
        uint32_t prefill_tokens = 1500,
        uint32_t pre_steps = 20,
        uint32_t post_steps = 20
    );

    [[nodiscard]] bool has_active_work() const;
    [[nodiscard]] size_t num_active_requests() const;
    [[nodiscard]] const SequenceRequest* get_request(uint32_t request_id) const;
    [[nodiscard]] const SplitFuseConfig& config() const noexcept { return config_; }
    [[nodiscard]] void* workspace_buffer() const noexcept { return d_workspace_; }
    [[nodiscard]] size_t workspace_size() const noexcept { return config_.preallocated_workspace_bytes; }

    void reset();

private:
    SplitFuseConfig config_;
    uint32_t next_request_id_{1};

    // Queues
    std::vector<SequenceRequest> requests_;
    std::deque<uint32_t> prefill_queue_;
    std::vector<uint32_t> active_decode_streams_;

    // Pre-allocated CUDA workspace buffer
    void* d_workspace_{nullptr};
    void* d_dummy_weights_{nullptr};
    void* d_dummy_kv_{nullptr};

    // Pre-allocated FusedBatch buffer to avoid hot loop reallocations
    FusedBatch reusable_batch_;

    // Performance telemetry counters
    std::vector<double> assembly_latencies_us_;

    void allocate_workspace();
    void free_workspace();
};

} // namespace hydrogen
