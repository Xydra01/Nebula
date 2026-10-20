// hydrogen/src/attention/dispatch.cpp
// Forward scheduler pre-sorting and templated attention dispatch (Issue H09)

#include "dispatch.hpp"
#include <cuda_runtime.h>
#include <algorithm>
#include <chrono>
#include <filesystem>
#include <map>
#include <stdexcept>
#include <iostream>

namespace hydrogen {

std::vector<HomogeneousBatch> AttentionDispatcher::presort_batch(
    std::span<const AttentionRequest> requests
) {
    if (requests.empty()) {
        return {};
    }

    // Map each quantization type to its corresponding bucket
    std::map<PageQuantType, HomogeneousBatch> buckets;

    for (const auto& req : requests) {
        auto& batch = buckets[req.quant_type];
        batch.quant_type = req.quant_type;
        batch.request_ids.push_back(req.request_id);
        batch.total_tokens += req.seq_len;
    }

    std::vector<HomogeneousBatch> homogeneous_batches;
    homogeneous_batches.reserve(buckets.size());

    // Deterministic order: FP8_E4M3, then Q8_0, then FP16
    for (auto& [qtype, batch] : buckets) {
        homogeneous_batches.push_back(std::move(batch));
    }

    return homogeneous_batches;
}

void AttentionDispatcher::inspect_binary_metrics(
    const std::string& binary_path,
    size_t& out_binary_size,
    double& out_load_time_ms
) {
    // 1. Measure file binary size
    std::error_code ec;
    auto size = std::filesystem::file_size(binary_path, ec);
    if (!ec) {
        out_binary_size = size;
    } else {
        out_binary_size = 0;
    }

    // 2. Measure CUDA driver initialization / primary context acquisition time
    auto start = std::chrono::high_resolution_clock::now();
    int device_count = 0;
    cudaError_t err = cudaGetDeviceCount(&device_count);
    if (err == cudaSuccess && device_count > 0) {
        cudaDeviceProp prop;
        cudaGetDeviceProperties(&prop, 0);
    }
    auto end = std::chrono::high_resolution_clock::now();

    out_load_time_ms = std::chrono::duration<double, std::milli>(end - start).count();
}

} // namespace hydrogen
