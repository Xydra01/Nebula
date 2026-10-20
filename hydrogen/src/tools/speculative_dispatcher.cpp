#include "tool_barrier.hpp"

#include <fstream>
#include <sstream>
#include <iostream>
#include <algorithm>
#include <thread>

namespace hydrogen::tools {

SpeculativeDispatcher::SpeculativeDispatcher() {
    // Allocate 1 MB pinned host memory buffer for zero-copy DMA to GPU
    pinned_host_buf_size_ = 1024 * 1024;
    cudaError_t err = cudaHostAlloc(&pinned_host_buf_, pinned_host_buf_size_, cudaHostAllocDefault);
    if (err != cudaSuccess) {
        // Fallback to standard aligned memory if CUDA driver not active
        pinned_host_buf_ = new char[pinned_host_buf_size_];
    }
    reset();
}

SpeculativeDispatcher::~SpeculativeDispatcher() {
    if (bg_io_future_.valid()) {
        try {
            bg_io_future_.wait();
        } catch (...) {}
    }
    if (pinned_host_buf_) {
        cudaFreeHost(pinned_host_buf_);
        pinned_host_buf_ = nullptr;
    }
}

void SpeculativeDispatcher::reset() {
    if (bg_io_future_.valid()) {
        try {
            bg_io_future_.wait();
        } catch (...) {}
    }
    state_.store(SpeculativeState::IDLE);
    accumulated_json_.clear();
    result_ = SpeculativeIoResult();
}

void SpeculativeDispatcher::set_file_reader_mock(std::function<std::string(const std::string&)> mock_reader) {
    file_reader_mock_ = std::move(mock_reader);
}

void SpeculativeDispatcher::set_file_writer_mock(std::function<bool(const std::string&, const std::string&)> mock_writer) {
    file_writer_mock_ = std::move(mock_writer);
}

void SpeculativeDispatcher::set_process_spawner_mock(std::function<bool(const std::string&, const std::vector<std::string>&)> mock_spawner) {
    process_spawner_mock_ = std::move(mock_spawner);
}

void SpeculativeDispatcher::feed_token(uint32_t /*token_id*/, const std::string& token_str) {
    feed_text(token_str);
}

void SpeculativeDispatcher::feed_text(const std::string& text) {
    accumulated_json_ += text;
    process_stream_buffer();
}

void SpeculativeDispatcher::process_stream_buffer() {
    SpeculativeState cur_state = state_.load();
    if (cur_state == SpeculativeState::ABORTED || cur_state == SpeculativeState::TURN_FINALIZED ||
        cur_state == SpeculativeState::APPROVED_AND_EXECUTED) {
        return;
    }

    // 1. Extract tool name if not already extracted
    if (result_.tool_name.empty()) {
        size_t name_pos = accumulated_json_.find("\"name\"");
        if (name_pos != std::string::npos) {
            size_t colon_pos = accumulated_json_.find(':', name_pos + 6);
            if (colon_pos != std::string::npos) {
                size_t q1 = accumulated_json_.find('\"', colon_pos);
                if (q1 != std::string::npos) {
                    size_t q2 = accumulated_json_.find('\"', q1 + 1);
                    if (q2 != std::string::npos) {
                        result_.tool_name = accumulated_json_.substr(q1 + 1, q2 - (q1 + 1));
                        result_.tier = get_tool_tier(result_.tool_name);
                        state_.store(SpeculativeState::PARSING_ARGS);
                    }
                }
            }
        }
    }

    if (result_.tool_name.empty()) {
        state_.store(SpeculativeState::PARSING_NAME);
        return;
    }

    // 2. Extract primary parameter and detect closing quotation mark
    if (result_.primary_arg.empty()) {
        std::vector<std::string> target_keys;
        if (result_.tool_name == "fs.read" || result_.tool_name == "fs.write" || result_.tool_name == "code.lookup") {
            target_keys = {"\"path\""};
        } else if (result_.tool_name == "shell.run") {
            target_keys = {"\"command\""};
        } else if (result_.tool_name == "task.finish") {
            target_keys = {"\"result\""};
        } else if (result_.tool_name == "memory.lookup") {
            target_keys = {"\"query\""};
        } else {
            target_keys = {"\"path\"", "\"command\"", "\"query\"", "\"result\""};
        }

        for (const auto& key : target_keys) {
            size_t kpos = accumulated_json_.find(key);
            if (kpos != std::string::npos) {
                size_t colon_pos = accumulated_json_.find(':', kpos + key.size());
                if (colon_pos != std::string::npos) {
                    size_t q1 = accumulated_json_.find('\"', colon_pos);
                    if (q1 != std::string::npos) {
                        size_t q2 = accumulated_json_.find('\"', q1 + 1);
                        if (q2 != std::string::npos) {
                            // Primary argument literal is completely closed!
                            result_.primary_arg = accumulated_json_.substr(q1 + 1, q2 - (q1 + 1));

                            // Check speculative eligibility against strict whitelist
                            if (is_speculative_eligible(result_.tool_name)) {
                                trigger_background_speculative_read(result_.primary_arg);
                            } else {
                                trigger_mutating_barrier();
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
}

void SpeculativeDispatcher::trigger_background_speculative_read(const std::string& path) {
    state_.store(SpeculativeState::ARG_READY_TRIGGERED);
    trigger_time_ = std::chrono::high_resolution_clock::now();
    result_.is_blocked_by_barrier = false;
    result_.side_effect_occurred = false;

    // Launch asynchronous I/O thread in background
    bg_io_future_ = std::async(std::launch::async, [this, path]() {
        state_.store(SpeculativeState::IO_IN_FLIGHT);

        std::string content;
        if (file_reader_mock_) {
            content = file_reader_mock_(path);
        } else {
            std::ifstream file(path, std::ios::binary);
            if (file) {
                std::ostringstream ss;
                ss << file.rdbuf();
                content = ss.str();
            } else {
                content = "// File not found or simulated: " + path;
            }
        }

        // Copy into host pinned buffer
        size_t bytes = std::min(content.size(), pinned_host_buf_size_ - 1);
        if (pinned_host_buf_) {
            std::memcpy(pinned_host_buf_, content.data(), bytes);
            pinned_host_buf_[bytes] = '\0';
        }

        result_.content_payload = content;
        result_.bytes_loaded = bytes;
        result_.is_ready = true;
        resolve_time_ = std::chrono::high_resolution_clock::now();
        state_.store(SpeculativeState::IO_RESOLVED);
    });
}

void SpeculativeDispatcher::trigger_mutating_barrier() {
    // Strictly block mutating operation! Zero premature disk or process side effects!
    state_.store(SpeculativeState::BLOCKED_MUTATING);
    result_.is_blocked_by_barrier = true;
    result_.side_effect_occurred = false;
}

bool SpeculativeDispatcher::finalize_turn(bool executor_approved) {
    if (bg_io_future_.valid()) {
        bg_io_future_.wait();
    }

    auto now = std::chrono::high_resolution_clock::now();
    if (result_.is_ready && trigger_time_.time_since_epoch().count() > 0) {
        float lead_time_ms = std::chrono::duration<float, std::milli>(now - trigger_time_).count();
        result_.early_trigger_lead_time_ms = lead_time_ms;
    }

    if (is_speculative_eligible(result_.tool_name)) {
        state_.store(SpeculativeState::TURN_FINALIZED);
        return true;
    }

    // Mutating tool branch
    if (!executor_approved) {
        // Blocked: executor did not grant permission or prompt cancelled
        result_.side_effect_occurred = false;
        state_.store(SpeculativeState::BLOCKED_MUTATING);
        return false;
    }

    // Approved: execute mutation safely after turn finalization
    if (result_.tool_name == "fs.write") {
        if (file_writer_mock_) {
            file_writer_mock_(result_.primary_arg, result_.content_payload);
        }
        result_.side_effect_occurred = true;
    } else if (result_.tool_name == "shell.run") {
        if (process_spawner_mock_) {
            process_spawner_mock_(result_.primary_arg, {});
        }
        result_.side_effect_occurred = true;
    }

    state_.store(SpeculativeState::APPROVED_AND_EXECUTED);
    return true;
}

void SpeculativeDispatcher::abort_turn() {
    state_.store(SpeculativeState::ABORTED);
    // Mutating operations remain completely discarded with 0 disk or process side effects
    result_.side_effect_occurred = false;
}

bool SpeculativeDispatcher::has_premature_side_effects() const {
    SpeculativeState s = state_.load();
    if (s == SpeculativeState::APPROVED_AND_EXECUTED) {
        return false; // Authorized post-turn execution
    }
    return result_.side_effect_occurred;
}

} // namespace hydrogen::tools
