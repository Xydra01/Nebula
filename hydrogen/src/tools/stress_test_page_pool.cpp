// hydrogen/src/tools/stress_test_page_pool.cpp
// Verification & stress test utility for Issue H06: Lock-Free Page Pool & Compaction Sweeper

#include "page_pool.hpp"
#include <iostream>
#include <iomanip>
#include <vector>
#include <thread>
#include <chrono>
#include <random>
#include <atomic>
#include <cassert>
#include <string>

namespace {

void print_help(const char* prog) {
    std::cout << "Usage: " << prog << " [options]\n\n"
              << "Options:\n"
              << "  --json         Output structured JSON telemetry\n"
              << "  --ops <N>      Total operations across worker threads (default: 500,000)\n"
              << "  --threads <N>  Number of concurrent worker threads (default: 16)\n"
              << "  -v, --verbose  Print detailed allocation milestones\n"
              << "  -h, --help     Display this help message\n";
}

} // anonymous namespace

int main(int argc, char* argv[]) {
    bool json_output = false;
    bool verbose = false;
    size_t total_ops = 500000;
    size_t num_threads = 16;

    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--json") {
            json_output = true;
        } else if (arg == "-v" || arg == "--verbose") {
            verbose = true;
        } else if (arg == "--ops" && i + 1 < argc) {
            total_ops = std::stoull(argv[++i]);
        } else if (arg == "--threads" && i + 1 < argc) {
            num_threads = std::stoull(argv[++i]);
        } else if (arg == "-h" || arg == "--help") {
            print_help(argv[0]);
            return 0;
        }
    }

    constexpr size_t POOL_CAPACITY = 65536; // 64K pages (~84.4 MiB managed)
    hydrogen::PagePoolConfig config;
    config.total_pages = POOL_CAPACITY;
    config.page_size_bytes = hydrogen::PAGE_BLOCK_TOTAL_BYTES;

    hydrogen::PagePool pool(config);

    // 1. Concurrent Stress Test: 16 threads allocating and freeing 500,000 pages
    std::atomic<size_t> total_allocated_ops{0};
    std::atomic<size_t> total_freed_ops{0};
    std::atomic<size_t> double_free_count{0};
    std::atomic<size_t> allocation_failures{0};

    size_t ops_per_thread = total_ops / num_threads;

    auto start_time = std::chrono::high_resolution_clock::now();

    std::vector<std::thread> workers;
    workers.reserve(num_threads);

    for (size_t t = 0; t < num_threads; ++t) {
        workers.emplace_back([&pool, ops_per_thread, t, &total_allocated_ops, &total_freed_ops, &double_free_count, &allocation_failures]() {
            std::mt19937_64 rng(1337 + t);
            std::uniform_int_distribution<size_t> batch_dist(1, 32);

            std::vector<uint32_t> held_pages;
            held_pages.reserve(1024);

            size_t local_ops = 0;
            while (local_ops < ops_per_thread) {
                size_t batch_size = batch_dist(rng);
                if (local_ops + batch_size > ops_per_thread) {
                    batch_size = ops_per_thread - local_ops;
                }

                // Allocate batch
                size_t allocated = pool.allocate_batch(batch_size, held_pages);
                total_allocated_ops.fetch_add(allocated, std::memory_order_relaxed);
                local_ops += allocated;

                if (allocated < batch_size) {
                    allocation_failures.fetch_add(batch_size - allocated, std::memory_order_relaxed);
                }

                // Free approximately half of held pages randomly to create realistic churn
                if (held_pages.size() >= 32) {
                    size_t free_count = held_pages.size() / 2;
                    for (size_t f = 0; f < free_count; ++f) {
                        uint32_t pid = held_pages.back();
                        held_pages.pop_back();
                        if (!pool.free_page(pid)) {
                            double_free_count.fetch_add(1, std::memory_order_relaxed);
                        } else {
                            total_freed_ops.fetch_add(1, std::memory_order_relaxed);
                        }
                    }
                }
            }

            // Cleanup remaining held pages
            for (uint32_t pid : held_pages) {
                if (!pool.free_page(pid)) {
                    double_free_count.fetch_add(1, std::memory_order_relaxed);
                } else {
                    total_freed_ops.fetch_add(1, std::memory_order_relaxed);
                }
            }
        });
    }

    for (auto& w : workers) {
        w.join();
    }

    auto end_time = std::chrono::high_resolution_clock::now();
    double elapsed_ns = static_cast<double>(
        std::chrono::duration_cast<std::chrono::nanoseconds>(end_time - start_time).count()
    );

    size_t total_page_events = total_allocated_ops.load() + total_freed_ops.load();
    double ns_per_page_event = total_page_events > 0 ? (elapsed_ns / static_cast<double>(total_page_events)) : 0.0;
    double ns_per_alloc = total_allocated_ops.load() > 0 ? (elapsed_ns / static_cast<double>(total_allocated_ops.load())) : 0.0;

    // Verify zero leakage after all threads finish
    size_t remaining_allocated = pool.allocated_pages_count();
    size_t final_free = pool.free_pages_count();

    bool latency_passed = (ns_per_page_event < 50.0);
    bool no_leaks = (remaining_allocated == 0 && final_free == POOL_CAPACITY);
    bool no_double_frees = (double_free_count.load() == 0);

    // 2. Compaction Sweeper Verification: create deliberate hole fragmentation
    std::vector<uint32_t> test_pages;
    pool.allocate_batch(1000, test_pages);
    assert(test_pages.size() == 1000);

    // Free every alternate page (500 holes created)
    for (size_t i = 0; i < 500; ++i) {
        pool.free_page(test_pages[i * 2]);
    }

    size_t pre_largest_run = 0, pre_holes = 0;
    double pre_frag = pool.calculate_fragmentation(pre_largest_run, pre_holes);

    std::vector<std::pair<uint32_t, uint32_t>> relocations;
    size_t compacted_count = pool.compact_pool(relocations);

    size_t post_largest_run = 0, post_holes = 0;
    double post_frag = pool.calculate_fragmentation(post_largest_run, post_holes);

    // Cleanup compaction test
    for (size_t i = 0; i < 500; ++i) {
        pool.free_page(static_cast<uint32_t>(i));
    }

    bool compaction_passed = (compacted_count > 0) && (post_frag == 0.0) && (post_holes <= 1);
    bool all_passed = latency_passed && no_leaks && no_double_frees && compaction_passed;

    if (json_output) {
        std::cout << "{\n"
                  << "  \"status\": \"" << (all_passed ? "PASS" : "FAIL") << "\",\n"
                  << "  \"pool_capacity_pages\": " << POOL_CAPACITY << ",\n"
                  << "  \"total_operations\": " << total_ops << ",\n"
                  << "  \"concurrent_threads\": " << num_threads << ",\n"
                  << "  \"total_allocated_pages\": " << total_allocated_ops.load() << ",\n"
                  << "  \"total_freed_pages\": " << total_freed_ops.load() << ",\n"
                  << "  \"ns_per_page_event\": " << std::fixed << std::setprecision(2) << ns_per_page_event << ",\n"
                  << "  \"ns_per_alloc_op\": " << std::fixed << std::setprecision(2) << ns_per_alloc << ",\n"
                  << "  \"latency_threshold_ns\": 50.0,\n"
                  << "  \"latency_under_50ns\": " << (latency_passed ? "true" : "false") << ",\n"
                  << "  \"remaining_allocated_pages\": " << remaining_allocated << ",\n"
                  << "  \"zero_leakage_verified\": " << (no_leaks ? "true" : "false") << ",\n"
                  << "  \"double_free_count\": " << double_free_count.load() << ",\n"
                  << "  \"pre_compaction_fragmentation\": " << std::fixed << std::setprecision(4) << pre_frag << ",\n"
                  << "  \"pre_compaction_holes\": " << pre_holes << ",\n"
                  << "  \"post_compaction_fragmentation\": " << std::fixed << std::setprecision(4) << post_frag << ",\n"
                  << "  \"compacted_pages_relocated\": " << compacted_count << ",\n"
                  << "  \"compaction_verified\": " << (compaction_passed ? "true" : "false") << "\n"
                  << "}\n";
    } else {
        std::cout << "\n===================================================\n"
                  << "Project Hydrogen: Lock-Free Page Pool Stress Test\n"
                  << "===================================================\n"
                  << "Pool Capacity:             " << POOL_CAPACITY << " pages ("
                  << (POOL_CAPACITY * hydrogen::PAGE_BLOCK_TOTAL_BYTES / 1024 / 1024) << " MiB)\n"
                  << "Concurrent Workers:        " << num_threads << " threads\n"
                  << "Total Operations:          " << total_ops << " events\n"
                  << "Allocated Operations:      " << total_allocated_ops.load() << "\n"
                  << "Freed Operations:          " << total_freed_ops.load() << "\n"
                  << "---------------------------------------------------\n"
                  << "Latency & Throughput:\n"
                  << "  Avg Latency per Event:   " << std::fixed << std::setprecision(2) << ns_per_page_event << " ns/page (Target: <50 ns) - "
                  << (latency_passed ? "PASS" : "FAIL") << "\n"
                  << "  Avg Latency per Alloc:   " << std::fixed << std::setprecision(2) << ns_per_alloc << " ns/page\n"
                  << "  Page Leakage / Retained: " << remaining_allocated << " pages - "
                  << (no_leaks ? "PASS (Zero Leaks)" : "FAIL") << "\n"
                  << "  Double-Free Errors:      " << double_free_count.load() << " - "
                  << (no_double_frees ? "PASS" : "FAIL") << "\n"
                  << "---------------------------------------------------\n"
                  << "VRAM Compaction Sweeper:\n"
                  << "  Pre-Compaction Holes:    " << pre_holes << " disjoint holes (Frag: "
                  << std::fixed << std::setprecision(1) << (pre_frag * 100.0) << "%)\n"
                  << "  Pages Relocated:         " << compacted_count << " leaf pages compacted\n"
                  << "  Post-Compaction Frag:    " << std::fixed << std::setprecision(1) << (post_frag * 100.0) << "% (Target: 0.0%) - "
                  << (compaction_passed ? "PASS" : "FAIL") << "\n"
                  << "===================================================\n"
                  << "Overall Status:            " << (all_passed ? "PASS (All Invariants Verified)" : "FAIL") << "\n\n";
    }

    return all_passed ? 0 : 1;
}
