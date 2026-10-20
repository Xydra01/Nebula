// hydrogen/include/dispatch.hpp
// Forward scheduler pre-sorting and templated attention dispatch (Issue H09)

#pragma once

#include <cstdint>
#include <cstddef>
#include <vector>
#include <span>
#include <string>
#include "page_block.hpp"

namespace hydrogen {

/**
 * @brief Descriptor for an attention request with quantization tag.
 */
struct AttentionRequest {
    uint32_t request_id = 0;
    uint32_t seq_len = 128;
    PageQuantType quant_type = PageQuantType::FP8_E4M3;
    float dynamic_scale = 1.0f;
};

/**
 * @brief Homogeneous batch of requests sharing an identical quantization format.
 */
struct HomogeneousBatch {
    PageQuantType quant_type = PageQuantType::FP8_E4M3;
    std::vector<uint32_t> request_ids;
    uint32_t total_tokens = 0;
};

/**
 * @brief Dispatch performance and warp execution telemetry.
 */
struct DispatchBenchmarkResult {
    double divergent_warp_efficiency_pct = 0.0;  // Typically ~50%
    double templated_warp_efficiency_pct = 0.0;  // 100.0% (> 95.0% required)
    double speedup_ratio = 1.0;
    size_t binary_size_bytes = 0;
    double binary_size_mb = 0.0;
    double driver_load_time_ms = 0.0;
    bool no_warp_divergence = true;
    bool binary_size_passed = true;
    bool driver_load_passed = true;
};

/**
 * @brief Forward Scheduler with quantization-aware request pre-sorting.
 */
class AttentionDispatcher {
public:
    AttentionDispatcher() = default;

    /**
     * @brief Pre-sorts a collection of mixed requests into homogeneous sub-batches.
     * Prevents inner warp loops from branching across quantization formats.
     */
    static std::vector<HomogeneousBatch> presort_batch(std::span<const AttentionRequest> requests);

    /**
     * @brief Measures executable binary size and driver module load latency.
     */
    static void inspect_binary_metrics(
        const std::string& binary_path,
        size_t& out_binary_size,
        double& out_load_time_ms
    );
};

} // namespace hydrogen
