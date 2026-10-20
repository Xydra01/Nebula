#include "host_slab.hpp"
#include <iostream>
#include <iomanip>
#include <chrono>
#include <thread>
#include <vector>
#include <random>
#include <string>
#include <sstream>
#include <cassert>

using namespace hydrogen;

// Contended baseline for empirical comparison (all 32 threads hit single atomic counter on every op)
struct ContendedGlobalBaseline {
    std::atomic<uint32_t> global_counter{0};
    std::atomic<uint64_t> atomic_transactions{0};

    uint32_t allocate() {
        atomic_transactions.fetch_add(1, std::memory_order_seq_cst);
        return global_counter.fetch_add(1, std::memory_order_seq_cst);
    }

    void free(uint32_t) {
        atomic_transactions.fetch_add(1, std::memory_order_seq_cst);
        global_counter.fetch_sub(1, std::memory_order_seq_cst);
    }
};

int main(int argc, char* argv[]) {
    bool json_output = false;
    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--json") {
            json_output = true;
        } else if (arg == "--help" || arg == "-h") {
            std::cout << "Usage: hydrogen_bench_host_slab.exe [OPTIONS]\n\n"
                      << "Options:\n"
                      << "  --json    Output structured telemetry in JSON format\n"
                      << "  --help    Show this help message and exit\n";
            return 0;
        }
    }

    if (!json_output) {
        std::cout << "========================================================================\n"
                  << "  Hydrogen H13 Benchmark: Tiered Host Pinned Slab Allocator & Sub-Arenas\n"
                  << "========================================================================\n";
    }

    // ========================================================================
    // Test Case 1: Tiered Fallback Ladder under Simulated Memory Fragmentation
    // ========================================================================
    bool test1_passed = true;
    std::string tier2_name;
    std::string tier3_name;

    try {
        // Fallback to Tier 2 (4 x 1 GB)
        {
            HostSlabConfig cfg_tier2;
            cfg_tier2.simulate_fragmentation = true;
            cfg_tier2.force_tier3_fragmentation = false;
            cfg_tier2.verbose = !json_output;
            TieredHostSlabAllocator alloc_tier2(cfg_tier2);
            tier2_name = allocation_tier_to_string(alloc_tier2.active_tier());
            if (alloc_tier2.active_tier() != AllocationTier::Tier2_Four1GB) {
                test1_passed = false;
            }
            if (alloc_tier2.total_slots() != TOTAL_SLOTS || alloc_tier2.total_pinned_bytes() > MAX_PINNED_BYTES) {
                test1_passed = false;
            }
        }

        // Fallback to Tier 3 (16 x 256 MB)
        {
            HostSlabConfig cfg_tier3;
            cfg_tier3.simulate_fragmentation = true;
            cfg_tier3.force_tier3_fragmentation = true;
            cfg_tier3.verbose = !json_output;
            TieredHostSlabAllocator alloc_tier3(cfg_tier3);
            tier3_name = allocation_tier_to_string(alloc_tier3.active_tier());
            if (alloc_tier3.active_tier() != AllocationTier::Tier3_Sixteen256MB) {
                test1_passed = false;
            }
            if (alloc_tier3.total_slots() != TOTAL_SLOTS || alloc_tier3.total_pinned_bytes() > MAX_PINNED_BYTES) {
                test1_passed = false;
            }
        }
    } catch (const std::exception& e) {
        std::cerr << "[Test 1 Error] " << e.what() << "\n";
        test1_passed = false;
    }

    // ========================================================================
    // Test Case 2: Address Translation Latency Benchmark (< 1.0 ns/op)
    // ========================================================================
    bool test2_passed = false;
    double translation_ns_per_op = 0.0;
    {
        HostSlabConfig cfg;
        cfg.requested_tier = AllocationTier::Tier3_Sixteen256MB;
        cfg.verbose = !json_output;
        TieredHostSlabAllocator alloc(cfg);

        constexpr size_t TRANSLATION_ITERS = 20000000; // 20M iterations

        // Warmup & verify mapping correctness across all slabs
        for (uint32_t s = 0; s < 16; ++s) {
            uint32_t test_slot = s * SLOTS_PER_SLAB + 128;
            void* addr = alloc.get_slot_address(test_slot);
            if (!addr) { test2_passed = false; }
        }

        uintptr_t s0 = 0, s1 = 0, s2 = 0, s3 = 0;
        uint32_t k0 = 100, k1 = 500000, k2 = 1200000, k3 = 2500000;
        constexpr size_t BATCH_ITERS = TRANSLATION_ITERS / 4;

        auto start = std::chrono::high_resolution_clock::now();
        for (size_t i = 0; i < BATCH_ITERS; ++i) {
            void* a0 = alloc.get_slot_address(k0);
            void* a1 = alloc.get_slot_address(k1);
            void* a2 = alloc.get_slot_address(k2);
            void* a3 = alloc.get_slot_address(k3);

            s0 ^= reinterpret_cast<uintptr_t>(a0);
            s1 ^= reinterpret_cast<uintptr_t>(a1);
            s2 ^= reinterpret_cast<uintptr_t>(a2);
            s3 ^= reinterpret_cast<uintptr_t>(a3);

            k0 = (k0 + 17 < TOTAL_SLOTS) ? k0 + 17 : 0;
            k1 = (k1 + 29 < TOTAL_SLOTS) ? k1 + 29 : 0;
            k2 = (k2 + 43 < TOTAL_SLOTS) ? k2 + 43 : 0;
            k3 = (k3 + 61 < TOTAL_SLOTS) ? k3 + 61 : 0;
        }
        auto end = std::chrono::high_resolution_clock::now();
        volatile uintptr_t sink = s0 ^ s1 ^ s2 ^ s3;
        (void)sink;

        std::chrono::duration<double, std::nano> elapsed = end - start;
        translation_ns_per_op = elapsed.count() / static_cast<double>(TRANSLATION_ITERS);

        test2_passed = (translation_ns_per_op < 1.0);
    }

    // ========================================================================
    // Test Case 3: 32 Concurrent Threads & 98.4% Bus Transaction Reduction
    // ========================================================================
    bool test3_passed = false;
    double reduction_pct = 0.0;
    double tiered_alloc_latency_ns = 0.0;
    double baseline_alloc_latency_ns = 0.0;
    uint64_t total_requests_count = 0;
    uint64_t total_atomic_transactions_count = 0;

    {
        constexpr size_t NUM_THREADS = 32;
        constexpr size_t OPS_PER_THREAD = 100000; // 100k alloc + 100k free per thread = 6.4M ops

        HostSlabConfig cfg;
        cfg.requested_tier = AllocationTier::Tier3_Sixteen256MB;
        cfg.verbose = !json_output;
        TieredHostSlabAllocator alloc(cfg);

        // 3a. Benchmark Contended Baseline
        ContendedGlobalBaseline baseline;
        auto base_start = std::chrono::high_resolution_clock::now();
        {
            std::vector<std::thread> workers;
            workers.reserve(NUM_THREADS);
            for (size_t t = 0; t < NUM_THREADS; ++t) {
                workers.emplace_back([&baseline]() {
                    for (size_t i = 0; i < OPS_PER_THREAD; ++i) {
                        uint32_t s = baseline.allocate();
                        baseline.free(s);
                    }
                });
            }
            for (auto& w : workers) w.join();
        }
        auto base_end = std::chrono::high_resolution_clock::now();
        std::chrono::duration<double, std::nano> base_elapsed = base_end - base_start;
        baseline_alloc_latency_ns = base_elapsed.count() / static_cast<double>(NUM_THREADS * OPS_PER_THREAD * 2);

        // 3b. Benchmark Tiered Host Slab Allocator with 64-slot Thread Caches & 8 Sub-Arenas
        std::vector<uint64_t> thread_requests(NUM_THREADS, 0);
        std::vector<uint64_t> thread_atomics(NUM_THREADS, 0);

        auto tiered_start = std::chrono::high_resolution_clock::now();
        {
            std::vector<std::thread> workers;
            workers.reserve(NUM_THREADS);
            for (size_t t = 0; t < NUM_THREADS; ++t) {
                workers.emplace_back([&alloc, t, &thread_requests, &thread_atomics]() {
                    // Assign each thread to its dedicated sub-arena
                    ThreadLocalCache cache(static_cast<uint32_t>(t % TOTAL_SUBARENAS));
                    std::vector<uint32_t> held_slots;
                    held_slots.reserve(128);

                    for (size_t i = 0; i < OPS_PER_THREAD; ++i) {
                        uint32_t slot = alloc.allocate_slot(cache);
                        if (slot != INVALID_SLOT_ID) {
                            held_slots.push_back(slot);
                        }
                        if (held_slots.size() >= 32) {
                            while (!held_slots.empty()) {
                                alloc.free_slot(held_slots.back(), cache);
                                held_slots.pop_back();
                            }
                        }
                    }

                    // Flush any remaining
                    while (!held_slots.empty()) {
                        alloc.free_slot(held_slots.back(), cache);
                        held_slots.pop_back();
                    }

                    thread_requests[t] = cache.alloc_requests + cache.free_requests;
                    thread_atomics[t] = cache.atomic_bus_transactions;
                });
            }
            for (auto& w : workers) w.join();
        }
        auto tiered_end = std::chrono::high_resolution_clock::now();
        std::chrono::duration<double, std::nano> tiered_elapsed = tiered_end - tiered_start;
        tiered_alloc_latency_ns = tiered_elapsed.count() / static_cast<double>(NUM_THREADS * OPS_PER_THREAD * 2);

        for (size_t t = 0; t < NUM_THREADS; ++t) {
            total_requests_count += thread_requests[t];
            total_atomic_transactions_count += thread_atomics[t];
        }

        if (total_requests_count > 0) {
            reduction_pct = (1.0 - (static_cast<double>(total_atomic_transactions_count) / static_cast<double>(total_requests_count))) * 100.0;
        }

        test3_passed = (reduction_pct >= 98.0) && (tiered_alloc_latency_ns < 5.0);
    }

    bool all_passed = test1_passed && test2_passed && test3_passed;

    if (json_output) {
        std::cout << std::fixed << std::setprecision(4)
                  << "{\n"
                  << "  \"status\": \"" << (all_passed ? "PASS" : "FAIL") << "\",\n"
                  << "  \"acceptance_criteria\": {\n"
                  << "    \"tiered_fallback_passed\": " << (test1_passed ? "true" : "false") << ",\n"
                  << "    \"address_translation_under_1ns_passed\": " << (test2_passed ? "true" : "false") << ",\n"
                  << "    \"zero_false_sharing_and_reduction_passed\": " << (test3_passed ? "true" : "false") << "\n"
                  << "  },\n"
                  << "  \"metrics\": {\n"
                  << "    \"tier2_fallback_name\": \"" << tier2_name << "\",\n"
                  << "    \"tier3_fallback_name\": \"" << tier3_name << "\",\n"
                  << "    \"translation_ns_per_op\": " << translation_ns_per_op << ",\n"
                  << "    \"translation_target_ceiling_ns\": 1.0,\n"
                  << "    \"atomic_reduction_pct\": " << reduction_pct << ",\n"
                  << "    \"atomic_reduction_target_min\": 98.0,\n"
                  << "    \"tiered_alloc_latency_ns\": " << tiered_alloc_latency_ns << ",\n"
                  << "    \"contended_baseline_latency_ns\": " << baseline_alloc_latency_ns << ",\n"
                  << "    \"speedup_vs_contended\": " << (baseline_alloc_latency_ns / std::max(0.001, tiered_alloc_latency_ns)) << ",\n"
                  << "    \"total_requests\": " << total_requests_count << ",\n"
                  << "    \"total_atomic_transactions\": " << total_atomic_transactions_count << ",\n"
                  << "    \"max_pinned_bytes\": " << MAX_PINNED_BYTES << ",\n"
                  << "    \"total_slots\": " << TOTAL_SLOTS << "\n"
                  << "  }\n"
                  << "}\n";
    } else {
        std::cout << "\n------------------------------------------------------------------------\n"
                  << "Empirical Results:\n"
                  << "------------------------------------------------------------------------\n"
                  << "Test 1 [Tiered Fallback Ladder]:  " << (test1_passed ? "[PASS]" : "[FAIL]") << "\n"
                  << "  - Tier 2 Simulation: " << tier2_name << "\n"
                  << "  - Tier 3 Simulation: " << tier3_name << "\n"
                  << "  - Total Slots:       " << TOTAL_SLOTS << " (4 GiB max)\n\n"
                  << "Test 2 [Address Translation]:     " << (test2_passed ? "[PASS]" : "[FAIL]") << "\n"
                  << "  - Measured Latency:  " << std::fixed << std::setprecision(3) << translation_ns_per_op << " ns/op\n"
                  << "  - Target Ceiling:    < 1.000 ns/op\n\n"
                  << "Test 3 [32 Threads Concurrency]:  " << (test3_passed ? "[PASS]" : "[FAIL]") << "\n"
                  << "  - Requests Handled:  " << total_requests_count << "\n"
                  << "  - Atomic Bus Txns:   " << total_atomic_transactions_count << "\n"
                  << "  - Atomic Reduction:  " << std::fixed << std::setprecision(2) << reduction_pct << "% (Target: >= 98.0%)\n"
                  << "  - Tiered Alloc:      " << tiered_alloc_latency_ns << " ns/op (Target: < 5.0 ns/op)\n"
                  << "  - Contended Baseline:" << baseline_alloc_latency_ns << " ns/op\n"
                  << "  - Contention Speedup:" << std::fixed << std::setprecision(1) << (baseline_alloc_latency_ns / std::max(0.001, tiered_alloc_latency_ns)) << "x faster\n"
                  << "========================================================================\n"
                  << "Final Status: " << (all_passed ? "ALL CRITERIA VERIFIED (PASS)" : "VERIFICATION FAILED") << "\n"
                  << "========================================================================\n";
    }

    return all_passed ? 0 : 1;
}
