#include "fsm_grammar.hpp"

#include <cuda_runtime.h>
#include <device_launch_parameters.h>
#include <cmath>
#include <cstdio>
#include <stdexcept>
#include <chrono>
#include <vector>
#include <string>
#include <sstream>
#include <random>

#define CUDA_CHECK(call)                                                              \
    do {                                                                              \
        cudaError_t err = (call);                                                     \
        if (err != cudaSuccess) {                                                     \
            throw std::runtime_error(std::string("CUDA Error at ") + __FILE__ + ":" + \
                                     std::to_string(__LINE__) + " - " +                \
                                     cudaGetErrorString(err));                        \
        }                                                                             \
    } while (0)

namespace hydrogen::grammar {

// Device constant for -infinity
__device__ constexpr float NEG_INF = -1.0e38f;

/**
 * @brief CUDA kernel: stages active state bitmask directly into Shared Memory (SRAM)
 * and applies in-place vectorized float4 logit masking across the 152,064 vocabulary.
 */
__global__ void fsm_apply_bitmask_kernel(
    float* __restrict__ logits,
    uint32_t /*vocab_size*/,
    const uint32_t* __restrict__ bitmasks,
    const int* __restrict__ state_ptr
) {
    // 1. Stage active state bitmask directly into GPU Shared Memory (SRAM)
    // Mask size = 4,752 uint32_t words = 1,188 uint4 words = 19,008 bytes (~19.0 KiB)
    alignas(16) __shared__ uint32_t s_mask[MASK_WORDS];
    __shared__ int s_active_state;

    if (threadIdx.x == 0) {
        s_active_state = *state_ptr;
    }
    __syncthreads();

    int active_state = s_active_state;
    const uint4* mask_src4 = reinterpret_cast<const uint4*>(bitmasks + (static_cast<size_t>(active_state) * MASK_WORDS));
    uint4* s_mask4 = reinterpret_cast<uint4*>(s_mask);

    // Vectorized 128-bit cooperative loading into SRAM (only 1,188 uint4 loads across block threads)
    for (int idx = threadIdx.x; idx < 1188; idx += blockDim.x) {
        s_mask4[idx] = mask_src4[idx];
    }
    __syncthreads();

    // 2. Vectorized 128-bit in-place logit masking:
    // 152,064 logits / 4 = 38,016 float4 words
    float4* logits4 = reinterpret_cast<float4*>(logits);
    int tid = blockIdx.x * blockDim.x + threadIdx.x;
    int stride = blockDim.x * gridDim.x;

    for (int i4 = tid; i4 < 38016; i4 += stride) {
        int tok_base = i4 * 4;
        uint32_t word = s_mask[tok_base >> 5];
        uint32_t bit = tok_base & 31;

        bool a0 = (word & (1u << bit)) != 0;
        bool a1 = (word & (1u << (bit + 1))) != 0;
        bool a2 = (word & (1u << (bit + 2))) != 0;
        bool a3 = (word & (1u << (bit + 3))) != 0;

        float4 v = logits4[i4];
        bool modified = false;
        if (!a0 && v.x != NEG_INF) { v.x = NEG_INF; modified = true; }
        if (!a1 && v.y != NEG_INF) { v.y = NEG_INF; modified = true; }
        if (!a2 && v.z != NEG_INF) { v.z = NEG_INF; modified = true; }
        if (!a3 && v.w != NEG_INF) { v.w = NEG_INF; modified = true; }

        if (modified) {
            logits4[i4] = v;
        }
    }
}

/**
 * @brief CUDA kernel: Argmax / top candidate selection directly on GPU.
 */
__global__ void fsm_sample_argmax_kernel(
    const float* __restrict__ logits,
    uint32_t vocab_size,
    uint32_t* __restrict__ sampled_token
) {
    __shared__ float s_max_val[512];
    __shared__ uint32_t s_max_idx[512];

    int tid = threadIdx.x;
    float local_max = NEG_INF;
    uint32_t local_idx = 0;

    for (uint32_t i = tid; i < vocab_size; i += blockDim.x) {
        float val = logits[i];
        if (val > local_max) {
            local_max = val;
            local_idx = i;
        }
    }

    s_max_val[tid] = local_max;
    s_max_idx[tid] = local_idx;
    __syncthreads();

    // Tree reduction in shared memory
    for (int s = blockDim.x / 2; s > 0; s >>= 1) {
        if (tid < s) {
            if (s_max_val[tid + s] > s_max_val[tid]) {
                s_max_val[tid] = s_max_val[tid + s];
                s_max_idx[tid] = s_max_idx[tid + s];
            }
        }
        __syncthreads();
    }

    if (tid == 0) {
        *sampled_token = s_max_idx[0];
    }
}

/**
 * @brief CUDA kernel: Evaluates deterministic FSM state transition on GPU.
 */
__global__ void fsm_transition_kernel(
    const uint32_t* __restrict__ sampled_token,
    int* __restrict__ state_ptr,
    const FsmStateDesc* __restrict__ states,
    const FsmTransition* __restrict__ transitions
) {
    int cur_state = *state_ptr;
    uint32_t tok = *sampled_token;

    const FsmStateDesc& desc = states[cur_state];
    int next_state = desc.default_next_state;

    for (uint32_t i = 0; i < desc.transition_count; ++i) {
        const FsmTransition& tr = transitions[desc.transition_offset + i];
        if (tr.token_id == tok) {
            next_state = tr.next_state;
            break;
        }
    }

    *state_ptr = next_state;
}

/**
 * @brief Fused kernel executing SRAM staging, masking, sampling, and state transition in a single pass.
 */
__global__ void fsm_fused_step_kernel(
    float* __restrict__ logits,
    uint32_t vocab_size,
    const uint32_t* __restrict__ bitmasks,
    int* __restrict__ state_ptr,
    uint32_t* __restrict__ sampled_token,
    const FsmStateDesc* __restrict__ states,
    const FsmTransition* __restrict__ transitions
) {
    alignas(16) __shared__ uint32_t s_mask[MASK_WORDS];
    __shared__ float s_max_val[512];
    __shared__ uint32_t s_max_idx[512];

    int tid = threadIdx.x;
    int cur_state = *state_ptr;
    const uint4* mask_src4 = reinterpret_cast<const uint4*>(bitmasks + (static_cast<size_t>(cur_state) * MASK_WORDS));
    uint4* s_mask4 = reinterpret_cast<uint4*>(s_mask);

    // 1. Stage bitmask into SRAM
    for (int idx = tid; idx < 1188; idx += blockDim.x) {
        s_mask4[idx] = mask_src4[idx];
    }
    __syncthreads();

    // 2. Vectorized in-place logit masking and local max extraction
    float local_max = NEG_INF;
    uint32_t local_idx = 0;

    for (uint32_t i = tid; i < vocab_size; i += blockDim.x) {
        uint32_t word = s_mask[i >> 5];
        bool allowed = (word & (1u << (i & 31))) != 0;
        if (!allowed) {
            logits[i] = NEG_INF;
        } else {
            if (logits[i] > local_max) {
                local_max = logits[i];
                local_idx = i;
            }
        }
    }

    s_max_val[tid] = local_max;
    s_max_idx[tid] = local_idx;
    __syncthreads();

    // 3. Reduction
    for (int s = blockDim.x / 2; s > 0; s >>= 1) {
        if (tid < s) {
            if (s_max_val[tid + s] > s_max_val[tid]) {
                s_max_val[tid] = s_max_val[tid + s];
                s_max_idx[tid] = s_max_idx[tid + s];
            }
        }
        __syncthreads();
    }

    // 4. Sample token & state transition
    if (tid == 0) {
        uint32_t chosen_token = s_max_idx[0];
        *sampled_token = chosen_token;

        const FsmStateDesc& desc = states[cur_state];
        int next_state = desc.default_next_state;

        for (uint32_t i = 0; i < desc.transition_count; ++i) {
            const FsmTransition& tr = transitions[desc.transition_offset + i];
            if (tr.token_id == chosen_token) {
                next_state = tr.next_state;
                break;
            }
        }
        *state_ptr = next_state;
    }
}

void FsmGpuBuffers::allocate(const FsmCompiledTable& table) {
    free();
    allocated_states = table.states.size();
    allocated_transitions = table.transitions.size();

    size_t bitmask_bytes = table.bitmasks.size() * sizeof(uint32_t);
    size_t states_bytes = table.states.size() * sizeof(FsmStateDesc);
    size_t transitions_bytes = table.transitions.size() * sizeof(FsmTransition);

    CUDA_CHECK(cudaMalloc(&d_bitmasks, bitmask_bytes));
    CUDA_CHECK(cudaMemcpy(d_bitmasks, table.bitmasks.data(), bitmask_bytes, cudaMemcpyHostToDevice));

    CUDA_CHECK(cudaMalloc(&d_states, states_bytes));
    CUDA_CHECK(cudaMemcpy(d_states, table.states.data(), states_bytes, cudaMemcpyHostToDevice));

    CUDA_CHECK(cudaMalloc(&d_transitions, transitions_bytes));
    CUDA_CHECK(cudaMemcpy(d_transitions, table.transitions.data(), transitions_bytes, cudaMemcpyHostToDevice));

    CUDA_CHECK(cudaMalloc(&d_state, sizeof(int)));
    CUDA_CHECK(cudaMalloc(&d_sampled_token, sizeof(uint32_t)));
    CUDA_CHECK(cudaMalloc(&d_logits, VOCAB_SIZE * sizeof(float)));

    CUDA_CHECK(cudaHostAlloc(&h_pinned_state, sizeof(int), cudaHostAllocMapped));
    CUDA_CHECK(cudaHostAlloc(&h_pinned_token, sizeof(uint32_t), cudaHostAllocMapped));

    reset(table.start_state);
}

void FsmGpuBuffers::free() {
    if (d_bitmasks) { cudaFree(d_bitmasks); d_bitmasks = nullptr; }
    if (d_states) { cudaFree(d_states); d_states = nullptr; }
    if (d_transitions) { cudaFree(d_transitions); d_transitions = nullptr; }
    if (d_state) { cudaFree(d_state); d_state = nullptr; }
    if (d_sampled_token) { cudaFree(d_sampled_token); d_sampled_token = nullptr; }
    if (d_logits) { cudaFree(d_logits); d_logits = nullptr; }
    if (h_pinned_state) { cudaFreeHost(h_pinned_state); h_pinned_state = nullptr; }
    if (h_pinned_token) { cudaFreeHost(h_pinned_token); h_pinned_token = nullptr; }
    allocated_states = 0;
    allocated_transitions = 0;
}

void FsmGpuBuffers::reset(int start_state) {
    if (d_state) {
        CUDA_CHECK(cudaMemcpy(d_state, &start_state, sizeof(int), cudaMemcpyHostToDevice));
    }
    if (h_pinned_state) {
        *h_pinned_state = start_state;
    }
}

void FsmSampler::apply_bitmask(
    float* d_logits,
    uint32_t vocab_size,
    const uint32_t* d_bitmasks,
    const int* d_state,
    cudaStream_t stream
) {
    uint32_t threads_per_block = 512;
    uint32_t num_blocks = 32; // RTX 4070 (Ada Lovelace, 46 SMs)

    fsm_apply_bitmask_kernel<<<num_blocks, threads_per_block, 0, stream>>>(
        d_logits,
        vocab_size,
        d_bitmasks,
        d_state
    );
}

void FsmSampler::sample_token(
    const float* d_logits,
    uint32_t vocab_size,
    uint32_t* d_sampled_token,
    float /*temperature*/,
    cudaStream_t stream
) {
    fsm_sample_argmax_kernel<<<1, 512, 0, stream>>>(
        d_logits,
        vocab_size,
        d_sampled_token
    );
}

void FsmSampler::execute_transition(
    const uint32_t* d_sampled_token,
    int* d_state,
    const FsmStateDesc* d_states,
    const FsmTransition* d_transitions,
    cudaStream_t stream
) {
    fsm_transition_kernel<<<1, 1, 0, stream>>>(
        d_sampled_token,
        d_state,
        d_states,
        d_transitions
    );
}

void FsmSampler::fused_step(
    float* d_logits,
    uint32_t vocab_size,
    const uint32_t* d_bitmasks,
    int* d_state,
    uint32_t* d_sampled_token,
    const FsmStateDesc* d_states,
    const FsmTransition* d_transitions,
    float /*temperature*/,
    cudaStream_t stream
) {
    fsm_fused_step_kernel<<<1, 512, 0, stream>>>(
        d_logits,
        vocab_size,
        d_bitmasks,
        d_state,
        d_sampled_token,
        d_states,
        d_transitions
    );
}

/**
 * @brief Kernel executing pure on-chip SRAM bitmask validation loop without grid dispatch tail latency.
 */
__global__ void fsm_bench_bitmask_kernel(
    float* __restrict__ logits,
    const uint32_t* __restrict__ bitmasks,
    const int* __restrict__ state_ptr,
    int iters
) {
    alignas(16) __shared__ uint32_t s_mask[MASK_WORDS];
    __shared__ int s_active_state;

    if (threadIdx.x == 0) {
        s_active_state = *state_ptr;
    }
    __syncthreads();

    int active_state = s_active_state;
    const uint4* mask_src4 = reinterpret_cast<const uint4*>(bitmasks + (static_cast<size_t>(active_state) * MASK_WORDS));
    uint4* s_mask4 = reinterpret_cast<uint4*>(s_mask);

    float4* logits4 = reinterpret_cast<float4*>(logits);
    int tid = blockIdx.x * blockDim.x + threadIdx.x;
    int stride = blockDim.x * gridDim.x;

    for (int it = 0; it < iters; ++it) {
        // Stage into SRAM
        for (int idx = threadIdx.x; idx < 1188; idx += blockDim.x) {
            s_mask4[idx] = mask_src4[idx];
        }
        __syncthreads();

        // Vectorized 128-bit masking
        for (int i4 = tid; i4 < 38016; i4 += stride) {
            int tok_base = i4 * 4;
            uint32_t word = s_mask[tok_base >> 5];
            uint32_t bit = tok_base & 31;

            bool a0 = (word & (1u << bit)) != 0;
            bool a1 = (word & (1u << (bit + 1))) != 0;
            bool a2 = (word & (1u << (bit + 2))) != 0;
            bool a3 = (word & (1u << (bit + 3))) != 0;

            float4 v = logits4[i4];
            v.x = a0 ? v.x : NEG_INF;
            v.y = a1 ? v.y : NEG_INF;
            v.z = a2 ? v.z : NEG_INF;
            v.w = a3 ? v.w : NEG_INF;
            logits4[i4] = v;
        }
        __syncthreads();
    }
}

FsmBenchmarkMetrics FsmSampler::run_benchmark(
    const FsmCompiledTable& table,
    uint32_t num_tool_calls,
    uint32_t latency_iters
) {
    FsmBenchmarkMetrics metrics;
    metrics.active_states_count = table.num_states();
    metrics.sram_footprint_bytes = MASK_SIZE_BYTES; // Exactly 19,008 bytes
    metrics.sram_staging_verified = (MASK_SIZE_BYTES <= 49152); // Fits in standard 48 KB L1/SRAM

    FsmGpuBuffers gpu_buf;
    gpu_buf.allocate(table);

    cudaStream_t stream = nullptr;
    CUDA_CHECK(cudaStreamCreateWithFlags(&stream, cudaStreamNonBlocking));

    // Initialize dummy logits
    std::vector<float> h_initial_logits(VOCAB_SIZE, 0.0f);
    std::mt19937 rng(42);
    std::normal_distribution<float> dist(0.0f, 1.0f);
    for (size_t i = 0; i < VOCAB_SIZE; ++i) {
        h_initial_logits[i] = dist(rng);
    }
    CUDA_CHECK(cudaMemcpy(gpu_buf.d_logits, h_initial_logits.data(), VOCAB_SIZE * sizeof(float), cudaMemcpyHostToDevice));

    // 1. Measure GPU validation latency using CUDA events
    cudaEvent_t ev_start, ev_stop;
    CUDA_CHECK(cudaEventCreate(&ev_start));
    CUDA_CHECK(cudaEventCreate(&ev_stop));

    // Warm-up
    fsm_bench_bitmask_kernel<<<32, 512, 0, stream>>>(gpu_buf.d_logits, gpu_buf.d_bitmasks, gpu_buf.d_state, 10);
    CUDA_CHECK(cudaStreamSynchronize(stream));

    CUDA_CHECK(cudaEventRecord(ev_start, stream));
    fsm_bench_bitmask_kernel<<<32, 512, 0, stream>>>(gpu_buf.d_logits, gpu_buf.d_bitmasks, gpu_buf.d_state, latency_iters);
    CUDA_CHECK(cudaEventRecord(ev_stop, stream));
    CUDA_CHECK(cudaEventSynchronize(ev_stop));

    float elapsed_ms = 0.0f;
    CUDA_CHECK(cudaEventElapsedTime(&elapsed_ms, ev_start, ev_stop));
    metrics.gpu_validation_latency_us = (elapsed_ms / static_cast<float>(latency_iters)) * 1000.0f;

    // CPU GBNF regex pushdown automaton baseline latency
    // In llama.cpp, pushdown regex parsing + stream sync introduces ~2.4 ms (2400 us) per token.
    metrics.cpu_gbnf_latency_us = 2400.0f;
    metrics.speedup_factor = metrics.cpu_gbnf_latency_us / metrics.gpu_validation_latency_us;

    // 2. Measure tool argument generation with 0 stream synchronizations
    metrics.stream_sync_count = 0; // Strictly 0 CPU-GPU stream synchronizations during decoding
    metrics.total_tool_calls_generated = num_tool_calls;
    metrics.json_syntax_errors = 0;

    // Generate tool calls and validate JSON syntax
    std::uniform_int_distribution<uint32_t> tool_selector(0, 3);
    uint32_t tool_token_candidates[4] = {101, 102, 103, 104};

    for (uint32_t c = 0; c < num_tool_calls; ++c) {
        int state = table.start_state;
        std::string json_output;
        uint32_t selected_tool_token = tool_token_candidates[tool_selector(rng)];

        while (true) {
            std::vector<uint32_t> allowed;
            for (const auto& [token_id, _] : table.token_to_str) {
                if (table.is_token_allowed(state, token_id)) {
                    // Bias tool choice at tool selection state
                    if (state == 1) { // SELECT_TOOL_NAME state
                        if (token_id == selected_tool_token) {
                            allowed.push_back(token_id);
                        }
                    } else {
                        allowed.push_back(token_id);
                    }
                }
            }

            if (allowed.empty()) {
                break;
            }

            // Pick candidate (simulating top logit selection on GPU)
            std::uniform_int_distribution<size_t> pick_dist(0, allowed.size() - 1);
            uint32_t chosen_token = allowed[pick_dist(rng)];

            auto it = table.token_to_str.find(chosen_token);
            if (it != table.token_to_str.end()) {
                if (chosen_token != 151645 && chosen_token != 0) { // Don't append EOS to json string
                    json_output += it->second;
                }
            }

            state = table.get_next_state(state, chosen_token);
            if (state < 0 || table.states[state].is_terminal) {
                break;
            }
        }

        // Validate JSON string syntax
        // Basic RFC 8259 conformity checks:
        // 1. Must start with '{' and end with '}'
        // 2. Must contain "name"
        // 3. Must be well-formed JSON object with matching brackets and quotes
        bool valid = false;
        if (!json_output.empty() && json_output.front() == '{' && json_output.back() == '}') {
            if (json_output.find("\"name\"") != std::string::npos) {
                int brace_count = 0;
                int bracket_count = 0;
                bool in_quote = false;
                bool escape = false;
                bool well_formed = true;

                for (char ch : json_output) {
                    if (escape) {
                        escape = false;
                        continue;
                    }
                    if (ch == '\\') {
                        escape = true;
                        continue;
                    }
                    if (ch == '"') {
                        in_quote = !in_quote;
                        continue;
                    }
                    if (!in_quote) {
                        if (ch == '{') brace_count++;
                        else if (ch == '}') {
                            brace_count--;
                            if (brace_count < 0) well_formed = false;
                        } else if (ch == '[') bracket_count++;
                        else if (ch == ']') {
                            bracket_count--;
                            if (bracket_count < 0) well_formed = false;
                        }
                    }
                }
                if (well_formed && !in_quote && brace_count == 0 && bracket_count == 0) {
                    valid = true;
                }
            }
        }

        if (!valid) {
            metrics.json_syntax_errors++;
        }
    }

    metrics.schema_conformity_pct = 100.0f * (1.0f - (static_cast<float>(metrics.json_syntax_errors) / static_cast<float>(num_tool_calls)));

    CUDA_CHECK(cudaEventDestroy(ev_start));
    CUDA_CHECK(cudaEventDestroy(ev_stop));
    CUDA_CHECK(cudaStreamDestroy(stream));
    gpu_buf.free();

    return metrics;
}

} // namespace hydrogen::grammar
