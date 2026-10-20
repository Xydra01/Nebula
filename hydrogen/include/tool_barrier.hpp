#pragma once

#include <string>
#include <vector>
#include <unordered_set>
#include <memory>
#include <future>
#include <atomic>
#include <chrono>
#include <functional>
#include <cuda_runtime.h>

namespace hydrogen::tools {

/**
 * @brief Tool permission tiers matching Nebula's permission model.
 */
enum class ToolTier {
    Tier0_ReadOnly,  // Speculative eligible: idempotent, read-only (fs.read, fs.list, git.status, code.lookup)
    Tier1_Workspace, // Mutating: fs.write, git.commit, git.checkout, code.patch
    Tier2_Process,   // Execution: shell.run, task.finish
    Tier3_System     // System admin / driver / registry modifications
};

/**
 * @brief State of the speculative tool parser and execution barrier.
 */
enum class SpeculativeState {
    IDLE,
    PARSING_NAME,
    PARSING_ARGS,
    ARG_READY_TRIGGERED, // Disambiguation point reached (closing quote of primary argument)
    IO_IN_FLIGHT,        // Background thread actively reading file into pinned host RAM
    IO_RESOLVED,         // Data resident in pinned host RAM before turn completes
    BLOCKED_MUTATING,    // Barrier actively holding mutating operation
    TURN_FINALIZED,      // Terminal token emitted and turn completed
    APPROVED_AND_EXECUTED,
    ABORTED              // Turn aborted, zero side-effects
};

/**
 * @brief Execution payload and timing metrics for a speculative tool invocation.
 */
struct SpeculativeIoResult {
    std::string tool_name;
    std::string primary_arg;
    std::string secondary_arg;
    std::string content_payload;
    size_t bytes_loaded = 0;
    bool is_ready = false;
    float early_trigger_lead_time_ms = 0.0f;
    bool is_blocked_by_barrier = false;
    bool side_effect_occurred = false;
    ToolTier tier = ToolTier::Tier0_ReadOnly;
};

/**
 * @brief Strict idempotency whitelist: checks if a tool is eligible for early background I/O.
 */
inline bool is_speculative_eligible(const std::string& tool_name) {
    static const std::unordered_set<std::string> whitelist = {
        "fs.read",
        "fs.list",
        "git.status",
        "code.lookup",
        "memory.lookup"
    };
    return whitelist.find(tool_name) != whitelist.end();
}

/**
 * @brief Determines permission tier for a given tool.
 */
inline ToolTier get_tool_tier(const std::string& tool_name) {
    if (is_speculative_eligible(tool_name)) {
        return ToolTier::Tier0_ReadOnly;
    }
    if (tool_name == "fs.write" || tool_name == "code.patch" || tool_name == "git.commit") {
        return ToolTier::Tier1_Workspace;
    }
    if (tool_name == "shell.run" || tool_name == "task.finish") {
        return ToolTier::Tier2_Process;
    }
    return ToolTier::Tier3_System;
}

/**
 * @brief Token-stream parameter extractor and mutating turn-finalization barrier.
 */
class SpeculativeDispatcher {
public:
    SpeculativeDispatcher();
    ~SpeculativeDispatcher();

    // Disable copy
    SpeculativeDispatcher(const SpeculativeDispatcher&) = delete;
    SpeculativeDispatcher& operator=(const SpeculativeDispatcher&) = delete;

    /**
     * @brief Feed next token chunk from generator loop.
     */
    void feed_token(uint32_t token_id, const std::string& token_str);

    /**
     * @brief Feed text segment directly into stream parser.
     */
    void feed_text(const std::string& text);

    /**
     * @brief Abort active turn (e.g. model hallucination or mid-turn cancel).
     */
    void abort_turn();

    /**
     * @brief Finalize turn upon emitting terminal token, conditionally verifying approval.
     */
    bool finalize_turn(bool executor_approved = false);

    /**
     * @brief Reset state for next inference turn.
     */
    void reset();

    /**
     * @brief Check whether any unauthorized or premature disk/process side effects occurred.
     */
    [[nodiscard]] bool has_premature_side_effects() const;

    [[nodiscard]] SpeculativeState get_state() const { return state_.load(); }
    [[nodiscard]] const SpeculativeIoResult& get_result() const { return result_; }
    [[nodiscard]] const std::string& get_accumulated_json() const { return accumulated_json_; }

    /**
     * @brief Provide mock filesystem hook for unit testing without modifying real disk.
     */
    void set_file_reader_mock(std::function<std::string(const std::string&)> mock_reader);
    void set_file_writer_mock(std::function<bool(const std::string&, const std::string&)> mock_writer);
    void set_process_spawner_mock(std::function<bool(const std::string&, const std::vector<std::string>&)> mock_spawner);

private:
    void process_stream_buffer();
    void trigger_background_speculative_read(const std::string& path);
    void trigger_mutating_barrier();

    std::atomic<SpeculativeState> state_{SpeculativeState::IDLE};
    std::string accumulated_json_;
    SpeculativeIoResult result_;

    std::future<void> bg_io_future_;
    std::chrono::high_resolution_clock::time_point trigger_time_;
    std::chrono::high_resolution_clock::time_point resolve_time_;

    std::function<std::string(const std::string&)> file_reader_mock_;
    std::function<bool(const std::string&, const std::string&)> file_writer_mock_;
    std::function<bool(const std::string&, const std::vector<std::string>&)> process_spawner_mock_;

    // Pinned host memory buffer for zero-wait Split-Fuse ingestion
    char* pinned_host_buf_ = nullptr;
    size_t pinned_host_buf_size_ = 0;
};

} // namespace hydrogen::tools
