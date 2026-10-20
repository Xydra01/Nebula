// hydrogen/src/attention/cascade_attention.cu
// FlashInfer SM 8.9 Cascade Attention kernel integration and online LSE rescaling

#include "cascade_attention.hpp"
#include <cuda_runtime.h>
#include <string>
#include <cmath>
#include <iostream>
#include <vector>
#include <stdexcept>
#include <random>
#include <algorithm>

namespace hydrogen {

namespace {

#define CUDA_CHECK(call) \
    do { \
        cudaError_t err = call; \
        if (err != cudaSuccess) { \
            std::string msg = "CUDA error at " + std::string(__FILE__) + ":" + \
                              std::to_string(__LINE__) + " -> " + cudaGetErrorString(err); \
            throw std::runtime_error(msg); \
        } \
    } while (0)

// Full 32-thread warp reduction
__device__ inline float warp_reduce_sum(float val) {
    #pragma unroll
    for (int offset = 16; offset > 0; offset /= 2) {
        val += __shfl_down_sync(0xFFFFFFFF, val, offset);
    }
    return __shfl_sync(0xFFFFFFFF, val, 0);
}

/**
 * @brief Reference Attention: Concatenated [Prefix; Leaf] single-pass online softmax.
 * Vectorized with float4 (128 elements = 32 threads, exactly 1 warp).
 */
__global__ void kernel_reference_attention(
    const float* __restrict__ Q,
    const float* __restrict__ K_prefix,
    const float* __restrict__ V_prefix,
    const float* __restrict__ K_leaf,
    const float* __restrict__ V_leaf,
    float* __restrict__ Out_ref,
    uint32_t batch_size,
    uint32_t num_q_heads,
    uint32_t num_kv_heads,
    uint32_t head_dim,
    uint32_t prefix_len,
    uint32_t leaf_len,
    float sm_scale
) {
    uint32_t q_head_idx = blockIdx.x;
    uint32_t batch_idx = blockIdx.y;

    if (q_head_idx >= num_q_heads || batch_idx >= batch_size) return;

    uint32_t lane = threadIdx.x; // 0..31 (one warp per head)
    if (lane >= 32) return;

    uint32_t gqa_ratio = num_q_heads / num_kv_heads;
    uint32_t kv_head_idx = q_head_idx / gqa_ratio;

    const float4* q_ptr = reinterpret_cast<const float4*>(Q);
    const float4* kp_ptr = reinterpret_cast<const float4*>(K_prefix);
    const float4* vp_ptr = reinterpret_cast<const float4*>(V_prefix);
    const float4* kl_ptr = reinterpret_cast<const float4*>(K_leaf);
    const float4* vl_ptr = reinterpret_cast<const float4*>(V_leaf);
    float4* out_ptr = reinterpret_cast<float4*>(Out_ref);

    size_t q_offset = (batch_idx * num_q_heads + q_head_idx) * 32 + lane;
    float4 q_val = q_ptr[q_offset];

    float m_i = -1e20f;
    float d_i = 0.0f;
    float4 acc = make_float4(0.0f, 0.0f, 0.0f, 0.0f);

    // 1. Process Shared Prefix Tokens
    for (uint32_t j = 0; j < prefix_len; ++j) {
        size_t kv_offset = (j * num_kv_heads + kv_head_idx) * 32 + lane;
        float4 k_val = kp_ptr[kv_offset];
        float prod = q_val.x * k_val.x + q_val.y * k_val.y + q_val.z * k_val.z + q_val.w * k_val.w;

        float score = warp_reduce_sum(prod) * sm_scale;

        float m_prev = m_i;
        m_i = fmaxf(m_prev, score);
        float alpha = expf(m_prev - m_i);
        float p = expf(score - m_i);

        d_i = d_i * alpha + p;

        float4 v_val = vp_ptr[kv_offset];
        acc.x = acc.x * alpha + p * v_val.x;
        acc.y = acc.y * alpha + p * v_val.y;
        acc.z = acc.z * alpha + p * v_val.z;
        acc.w = acc.w * alpha + p * v_val.w;
    }

    // 2. Process Private Leaf Tokens
    size_t leaf_batch_base = batch_idx * leaf_len;
    for (uint32_t j = 0; j < leaf_len; ++j) {
        size_t kv_offset = ((leaf_batch_base + j) * num_kv_heads + kv_head_idx) * 32 + lane;
        float4 k_val = kl_ptr[kv_offset];
        float prod = q_val.x * k_val.x + q_val.y * k_val.y + q_val.z * k_val.z + q_val.w * k_val.w;

        float score = warp_reduce_sum(prod) * sm_scale;

        float m_prev = m_i;
        m_i = fmaxf(m_prev, score);
        float alpha = expf(m_prev - m_i);
        float p = expf(score - m_i);

        d_i = d_i * alpha + p;

        float4 v_val = vl_ptr[kv_offset];
        acc.x = acc.x * alpha + p * v_val.x;
        acc.y = acc.y * alpha + p * v_val.y;
        acc.z = acc.z * alpha + p * v_val.z;
        acc.w = acc.w * alpha + p * v_val.w;
    }

    // Write normalized reference output
    float inv_d = (d_i > 1e-12f) ? (1.0f / d_i) : 0.0f;
    out_ptr[q_offset] = make_float4(acc.x * inv_d, acc.y * inv_d, acc.z * inv_d, acc.w * inv_d);
}

/**
 * @brief Stage 1: Shared Prefix Attention Kernel.
 * Evaluates attention over shared prefix KV pages once for all batch sequences simultaneously.
 * Uses a 2D block (32, batch_size) where each sequence is handled by a dedicated warp (32 threads).
 * Prefix KV memory is loaded from VRAM exactly ONCE into shared memory across the batch.
 */
__global__ void kernel_cascade_stage1_prefix(
    const float* __restrict__ Q,
    const float* __restrict__ K_prefix,
    const float* __restrict__ V_prefix,
    float* __restrict__ Out_prefix,
    float* __restrict__ M_prefix,
    float* __restrict__ D_prefix,
    uint32_t batch_size,
    uint32_t num_q_heads,
    uint32_t num_kv_heads,
    uint32_t head_dim,
    uint32_t prefix_len,
    float sm_scale
) {
    uint32_t q_head_idx = blockIdx.x;
    if (q_head_idx >= num_q_heads) return;

    uint32_t lane = threadIdx.x; // 0..31 (head dimension 128 / 4)
    uint32_t b = threadIdx.y;    // 0..batch_size-1 (sequence index)

    if (lane >= 32 || b >= batch_size) return;

    uint32_t gqa_ratio = num_q_heads / num_kv_heads;
    uint32_t kv_head_idx = q_head_idx / gqa_ratio;

    const float4* q_ptr = reinterpret_cast<const float4*>(Q);
    const float4* kp_ptr = reinterpret_cast<const float4*>(K_prefix);
    const float4* vp_ptr = reinterpret_cast<const float4*>(V_prefix);
    float4* out_ptr = reinterpret_cast<float4*>(Out_prefix);

    __shared__ float4 s_k[8][32];
    __shared__ float4 s_v[8][32];

    size_t q_offset = (b * num_q_heads + q_head_idx) * 32 + lane;
    float4 q_val = q_ptr[q_offset];

    float m_i = -1e20f;
    float d_i = 0.0f;
    float4 acc = make_float4(0.0f, 0.0f, 0.0f, 0.0f);

    // Stream prefix KV tokens in tiles of 8 tokens
    for (uint32_t j = 0; j < prefix_len; j += 8) {
        // Load tile of 8 tokens into shared memory
        // Distribute load among warps: warp b loads tokens where (t % batch_size == b)
        #pragma unroll
        for (uint32_t t = 0; t < 8; ++t) {
            if (b == (t % batch_size)) {
                size_t kv_offset = ((j + t) * num_kv_heads + kv_head_idx) * 32 + lane;
                s_k[t][lane] = kp_ptr[kv_offset];
                s_v[t][lane] = vp_ptr[kv_offset];
            }
        }
        __syncthreads();

        // Process all 8 tokens in tile
        #pragma unroll
        for (uint32_t t = 0; t < 8; ++t) {
            float4 k_val = s_k[t][lane];
            float prod = q_val.x * k_val.x + q_val.y * k_val.y + q_val.z * k_val.z + q_val.w * k_val.w;

            float score = warp_reduce_sum(prod) * sm_scale;

            float m_prev = m_i;
            m_i = fmaxf(m_prev, score);
            float alpha = expf(m_prev - m_i);
            float p = expf(score - m_i);

            d_i = d_i * alpha + p;

            float4 v_val = s_v[t][lane];
            acc.x = acc.x * alpha + p * v_val.x;
            acc.y = acc.y * alpha + p * v_val.y;
            acc.z = acc.z * alpha + p * v_val.z;
            acc.w = acc.w * alpha + p * v_val.w;
        }
        __syncthreads();
    }

    uint32_t seq_head_idx = b * num_q_heads + q_head_idx;
    if (lane == 0) {
        M_prefix[seq_head_idx] = m_i;
        D_prefix[seq_head_idx] = d_i;
    }
    out_ptr[q_offset] = acc;
}

/**
 * @brief Stage 2: Private Leaf Attention Kernel.
 * Evaluates attention over unique private leaf pages per sequence.
 */
__global__ void kernel_cascade_stage2_leaf(
    const float* __restrict__ Q,
    const float* __restrict__ K_leaf,
    const float* __restrict__ V_leaf,
    float* __restrict__ Out_leaf,
    float* __restrict__ M_leaf,
    float* __restrict__ D_leaf,
    uint32_t batch_size,
    uint32_t num_q_heads,
    uint32_t num_kv_heads,
    uint32_t head_dim,
    uint32_t leaf_len,
    float sm_scale
) {
    uint32_t q_head_idx = blockIdx.x;
    uint32_t batch_idx = blockIdx.y;

    if (q_head_idx >= num_q_heads || batch_idx >= batch_size) return;

    uint32_t lane = threadIdx.x; // 0..31
    if (lane >= 32) return;

    uint32_t gqa_ratio = num_q_heads / num_kv_heads;
    uint32_t kv_head_idx = q_head_idx / gqa_ratio;

    const float4* q_ptr = reinterpret_cast<const float4*>(Q);
    const float4* kl_ptr = reinterpret_cast<const float4*>(K_leaf);
    const float4* vl_ptr = reinterpret_cast<const float4*>(V_leaf);
    float4* out_ptr = reinterpret_cast<float4*>(Out_leaf);

    size_t q_offset = (batch_idx * num_q_heads + q_head_idx) * 32 + lane;
    float4 q_val = q_ptr[q_offset];

    float m_i = -1e20f;
    float d_i = 0.0f;
    float4 acc = make_float4(0.0f, 0.0f, 0.0f, 0.0f);

    size_t leaf_batch_base = batch_idx * leaf_len;
    for (uint32_t j = 0; j < leaf_len; ++j) {
        size_t kv_offset = ((leaf_batch_base + j) * num_kv_heads + kv_head_idx) * 32 + lane;
        float4 k_val = kl_ptr[kv_offset];
        float prod = q_val.x * k_val.x + q_val.y * k_val.y + q_val.z * k_val.z + q_val.w * k_val.w;

        float score = warp_reduce_sum(prod) * sm_scale;

        float m_prev = m_i;
        m_i = fmaxf(m_prev, score);
        float alpha = expf(m_prev - m_i);
        float p = expf(score - m_i);

        d_i = d_i * alpha + p;

        float4 v_val = vl_ptr[kv_offset];
        acc.x = acc.x * alpha + p * v_val.x;
        acc.y = acc.y * alpha + p * v_val.y;
        acc.z = acc.z * alpha + p * v_val.z;
        acc.w = acc.w * alpha + p * v_val.w;
    }

    uint32_t seq_head_idx = batch_idx * num_q_heads + q_head_idx;
    if (lane == 0) {
        M_leaf[seq_head_idx] = m_i;
        D_leaf[seq_head_idx] = d_i;
    }
    out_ptr[q_offset] = acc;
}

/**
 * @brief Stage 3: Fused Online Log-Sum-Exp (LSE) Rescaling & Output Merge Kernel.
 */
__global__ void kernel_cascade_stage3_lse_merge(
    const float* __restrict__ Out_prefix,
    const float* __restrict__ M_prefix,
    const float* __restrict__ D_prefix,
    const float* __restrict__ Out_leaf,
    const float* __restrict__ M_leaf,
    const float* __restrict__ D_leaf,
    float* __restrict__ Out_cascade,
    uint32_t batch_size,
    uint32_t num_q_heads,
    uint32_t head_dim
) {
    uint32_t q_head_idx = blockIdx.x;
    uint32_t batch_idx = blockIdx.y;

    if (q_head_idx >= num_q_heads || batch_idx >= batch_size) return;

    uint32_t lane = threadIdx.x;
    if (lane >= 32) return;

    uint32_t seq_head_idx = batch_idx * num_q_heads + q_head_idx;
    size_t out_offset = seq_head_idx * 32 + lane;

    float m_p = M_prefix[seq_head_idx];
    float d_p = D_prefix[seq_head_idx];
    float4 acc_p = reinterpret_cast<const float4*>(Out_prefix)[out_offset];

    float m_l = M_leaf[seq_head_idx];
    float d_l = D_leaf[seq_head_idx];
    float4 acc_l = reinterpret_cast<const float4*>(Out_leaf)[out_offset];

    float m_total = fmaxf(m_p, m_l);
    float alpha_p = expf(m_p - m_total);
    float alpha_l = expf(m_l - m_total);

    float den = d_p * alpha_p + d_l * alpha_l;
    float inv_den = (den > 1e-12f) ? (1.0f / den) : 0.0f;

    float4 res;
    res.x = (acc_p.x * alpha_p + acc_l.x * alpha_l) * inv_den;
    res.y = (acc_p.y * alpha_p + acc_l.y * alpha_l) * inv_den;
    res.z = (acc_p.z * alpha_p + acc_l.z * alpha_l) * inv_den;
    res.w = (acc_p.w * alpha_p + acc_l.w * alpha_l) * inv_den;

    reinterpret_cast<float4*>(Out_cascade)[out_offset] = res;
}

} // anonymous namespace

CascadeAttentionEngine::CascadeAttentionEngine(CascadeAttentionConfig config)
    : config_(config) {
    allocate_device_memory();
    initialize_synthetic_data();
}

CascadeAttentionEngine::~CascadeAttentionEngine() {
    free_device_memory();
}

void CascadeAttentionEngine::allocate_device_memory() {
    size_t max_b = config_.batch_size;
    size_t q_elems = max_b * config_.num_q_heads * config_.head_dim;
    size_t prefix_kv_elems = config_.prefix_len * config_.num_kv_heads * config_.head_dim;
    size_t leaf_kv_elems = max_b * config_.leaf_len * config_.num_kv_heads * config_.head_dim;
    size_t out_elems = q_elems;
    size_t meta_elems = max_b * config_.num_q_heads;

    CUDA_CHECK(cudaMalloc(&d_q_, q_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_prefix_k_, prefix_kv_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_prefix_v_, prefix_kv_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_leaf_k_, leaf_kv_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_leaf_v_, leaf_kv_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_out_ref_, out_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_out_cascade_, out_elems * sizeof(float)));

    CUDA_CHECK(cudaMalloc(&d_stage1_out_, out_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_stage1_m_, meta_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_stage1_d_, meta_elems * sizeof(float)));

    CUDA_CHECK(cudaMalloc(&d_stage2_out_, out_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_stage2_m_, meta_elems * sizeof(float)));
    CUDA_CHECK(cudaMalloc(&d_stage2_d_, meta_elems * sizeof(float)));
}

void CascadeAttentionEngine::free_device_memory() {
    if (d_q_) cudaFree(d_q_);
    if (d_prefix_k_) cudaFree(d_prefix_k_);
    if (d_prefix_v_) cudaFree(d_prefix_v_);
    if (d_leaf_k_) cudaFree(d_leaf_k_);
    if (d_leaf_v_) cudaFree(d_leaf_v_);
    if (d_out_ref_) cudaFree(d_out_ref_);
    if (d_out_cascade_) cudaFree(d_out_cascade_);

    if (d_stage1_out_) cudaFree(d_stage1_out_);
    if (d_stage1_m_) cudaFree(d_stage1_m_);
    if (d_stage1_d_) cudaFree(d_stage1_d_);

    if (d_stage2_out_) cudaFree(d_stage2_out_);
    if (d_stage2_m_) cudaFree(d_stage2_m_);
    if (d_stage2_d_) cudaFree(d_stage2_d_);
}

void CascadeAttentionEngine::initialize_synthetic_data() {
    std::mt19937 rng(42);
    std::normal_distribution<float> dist(0.0f, 0.5f);

    size_t max_b = config_.batch_size;
    size_t q_elems = max_b * config_.num_q_heads * config_.head_dim;
    size_t prefix_kv_elems = config_.prefix_len * config_.num_kv_heads * config_.head_dim;
    size_t leaf_kv_elems = max_b * config_.leaf_len * config_.num_kv_heads * config_.head_dim;

    std::vector<float> h_q(q_elems);
    for (float& f : h_q) f = dist(rng);

    std::vector<float> h_prefix_k(prefix_kv_elems);
    for (float& f : h_prefix_k) f = dist(rng);

    std::vector<float> h_prefix_v(prefix_kv_elems);
    for (float& f : h_prefix_v) f = dist(rng);

    std::vector<float> h_leaf_k(leaf_kv_elems);
    for (float& f : h_leaf_k) f = dist(rng);

    std::vector<float> h_leaf_v(leaf_kv_elems);
    for (float& f : h_leaf_v) f = dist(rng);

    CUDA_CHECK(cudaMemcpy(d_q_, h_q.data(), q_elems * sizeof(float), cudaMemcpyHostToDevice));
    CUDA_CHECK(cudaMemcpy(d_prefix_k_, h_prefix_k.data(), prefix_kv_elems * sizeof(float), cudaMemcpyHostToDevice));
    CUDA_CHECK(cudaMemcpy(d_prefix_v_, h_prefix_v.data(), prefix_kv_elems * sizeof(float), cudaMemcpyHostToDevice));
    CUDA_CHECK(cudaMemcpy(d_leaf_k_, h_leaf_k.data(), leaf_kv_elems * sizeof(float), cudaMemcpyHostToDevice));
    CUDA_CHECK(cudaMemcpy(d_leaf_v_, h_leaf_v.data(), leaf_kv_elems * sizeof(float), cudaMemcpyHostToDevice));
}

void CascadeAttentionEngine::forward_reference(
    const float* d_q,
    const float* d_prefix_k,
    const float* d_prefix_v,
    const float* d_leaf_k,
    const float* d_leaf_v,
    float* d_out_ref,
    uint32_t batch_size
) {
    dim3 grid(config_.num_q_heads, batch_size);
    dim3 block(32); // 1 warp (32 threads) processes 128-dim head via float4
    float sm_scale = 1.0f / std::sqrt(static_cast<float>(config_.head_dim));

    kernel_reference_attention<<<grid, block>>>(
        d_q,
        d_prefix_k,
        d_prefix_v,
        d_leaf_k,
        d_leaf_v,
        d_out_ref,
        batch_size,
        config_.num_q_heads,
        config_.num_kv_heads,
        config_.head_dim,
        config_.prefix_len,
        config_.leaf_len,
        sm_scale
    );
    CUDA_CHECK(cudaGetLastError());
}

void CascadeAttentionEngine::forward_cascade(
    const float* d_q,
    const float* d_prefix_k,
    const float* d_prefix_v,
    const float* d_leaf_k,
    const float* d_leaf_v,
    float* d_out_cascade,
    uint32_t batch_size
) {
    float sm_scale = 1.0f / std::sqrt(static_cast<float>(config_.head_dim));

    // Stage 1: Shared Prefix Batched Projection
    dim3 grid_stage1(config_.num_q_heads, 1);
    dim3 block_stage1(32, batch_size);

    kernel_cascade_stage1_prefix<<<grid_stage1, block_stage1>>>(
        d_q,
        d_prefix_k,
        d_prefix_v,
        d_stage1_out_,
        d_stage1_m_,
        d_stage1_d_,
        batch_size,
        config_.num_q_heads,
        config_.num_kv_heads,
        config_.head_dim,
        config_.prefix_len,
        sm_scale
    );
    CUDA_CHECK(cudaGetLastError());

    // Stage 2: Private Leaf Paged Projection
    dim3 grid_stage2(config_.num_q_heads, batch_size);
    dim3 block_stage2(32);

    kernel_cascade_stage2_leaf<<<grid_stage2, block_stage2>>>(
        d_q,
        d_leaf_k,
        d_leaf_v,
        d_stage2_out_,
        d_stage2_m_,
        d_stage2_d_,
        batch_size,
        config_.num_q_heads,
        config_.num_kv_heads,
        config_.head_dim,
        config_.leaf_len,
        sm_scale
    );
    CUDA_CHECK(cudaGetLastError());

    // Stage 3: Fused Online LSE Rescaling & Output Merge
    dim3 grid_stage3(config_.num_q_heads, batch_size);
    dim3 block_stage3(32);

    kernel_cascade_stage3_lse_merge<<<grid_stage3, block_stage3>>>(
        d_stage1_out_,
        d_stage1_m_,
        d_stage1_d_,
        d_stage2_out_,
        d_stage2_m_,
        d_stage2_d_,
        d_out_cascade,
        batch_size,
        config_.num_q_heads,
        config_.head_dim
    );
    CUDA_CHECK(cudaGetLastError());
}

void CascadeAttentionEngine::verify_fidelity(
    const float* d_out_ref,
    const float* d_out_cascade,
    uint32_t batch_size,
    double& out_max_abs_err,
    double& out_mse
) {
    size_t count = batch_size * config_.num_q_heads * config_.head_dim;
    std::vector<float> h_ref(count);
    std::vector<float> h_cascade(count);

    CUDA_CHECK(cudaMemcpy(h_ref.data(), d_out_ref, count * sizeof(float), cudaMemcpyDeviceToHost));
    CUDA_CHECK(cudaMemcpy(h_cascade.data(), d_out_cascade, count * sizeof(float), cudaMemcpyDeviceToHost));

    double max_err = 0.0;
    double sum_sq = 0.0;

    for (size_t i = 0; i < count; ++i) {
        double diff = std::abs(static_cast<double>(h_ref[i]) - static_cast<double>(h_cascade[i]));
        if (diff > max_err) max_err = diff;
        sum_sq += diff * diff;
    }

    out_max_abs_err = max_err;
    out_mse = sum_sq / static_cast<double>(count);
}

std::vector<CascadeBenchmarkResult> CascadeAttentionEngine::run_benchmark_sweep(
    int warmups,
    int iterations
) {
    std::vector<CascadeBenchmarkResult> results;
    std::vector<uint32_t> batch_sizes = {1, 2, 4, 8};

    cudaEvent_t start, stop;
    CUDA_CHECK(cudaEventCreate(&start));
    CUDA_CHECK(cudaEventCreate(&stop));

    // Measure single-stream reference attention baseline
    for (int w = 0; w < warmups; ++w) {
        forward_reference(d_q_, d_prefix_k_, d_prefix_v_, d_leaf_k_, d_leaf_v_, d_out_ref_, 1);
    }
    CUDA_CHECK(cudaDeviceSynchronize());

    CUDA_CHECK(cudaEventRecord(start));
    for (int it = 0; it < iterations; ++it) {
        forward_reference(d_q_, d_prefix_k_, d_prefix_v_, d_leaf_k_, d_leaf_v_, d_out_ref_, 1);
    }
    CUDA_CHECK(cudaEventRecord(stop));
    CUDA_CHECK(cudaEventSynchronize(stop));

    float ref_ms = 0.0f;
    CUDA_CHECK(cudaEventElapsedTime(&ref_ms, start, stop));
    float baseline_latency = ref_ms / static_cast<float>(iterations);
    double baseline_throughput = 1000.0 / static_cast<double>(baseline_latency);

    for (uint32_t b : batch_sizes) {
        // 1. Verify numerical fidelity for this batch size
        forward_reference(d_q_, d_prefix_k_, d_prefix_v_, d_leaf_k_, d_leaf_v_, d_out_ref_, b);
        forward_cascade(d_q_, d_prefix_k_, d_prefix_v_, d_leaf_k_, d_leaf_v_, d_out_cascade_, b);
        CUDA_CHECK(cudaDeviceSynchronize());

        double max_err = 0.0, mse = 0.0;
        verify_fidelity(d_out_ref_, d_out_cascade_, b, max_err, mse);

        // 2. Warmup
        for (int w = 0; w < warmups; ++w) {
            forward_cascade(d_q_, d_prefix_k_, d_prefix_v_, d_leaf_k_, d_leaf_v_, d_out_cascade_, b);
        }
        CUDA_CHECK(cudaDeviceSynchronize());

        // 3. Timed benchmark runs
        CUDA_CHECK(cudaEventRecord(start));
        for (int it = 0; it < iterations; ++it) {
            forward_cascade(d_q_, d_prefix_k_, d_prefix_v_, d_leaf_k_, d_leaf_v_, d_out_cascade_, b);
        }
        CUDA_CHECK(cudaEventRecord(stop));
        CUDA_CHECK(cudaEventSynchronize(stop));

        float total_ms = 0.0f;
        CUDA_CHECK(cudaEventElapsedTime(&total_ms, start, stop));
        float avg_ms = total_ms / static_cast<float>(iterations);

        CascadeBenchmarkResult res;
        res.batch_size = b;
        res.latency_ms = avg_ms;
        res.throughput_tok_s = (static_cast<double>(b) * 1000.0) / static_cast<double>(avg_ms);
        res.speedup_ratio = (baseline_throughput > 0.0) ?
            (res.throughput_tok_s / baseline_throughput) : 1.0;
        res.max_abs_error = max_err;
        res.mean_squared_error = mse;
        res.numerical_match = (max_err < 1e-4);

        results.push_back(res);
    }

    CUDA_CHECK(cudaEventDestroy(start));
    CUDA_CHECK(cudaEventDestroy(stop));

    return results;
}

} // namespace hydrogen
