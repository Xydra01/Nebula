// hydrogen/src/tools/test_radix_tree.cpp
// Verification tool for Issue H05: Radix-Tree Paged KV Cache & Copy-on-Write Branching

#include "radix_tree.hpp"
#include <iostream>
#include <iomanip>
#include <vector>
#include <string>
#include <thread>
#include <cassert>
#include <cmath>

namespace {

void print_help(const char* prog) {
    std::cout << "Usage: " << prog << " [options]\n\n"
              << "Options:\n"
              << "  --json       Output structured JSON telemetry\n"
              << "  -v, --verbose Print detailed tree topology\n"
              << "  -h, --help   Display this help message\n";
}

} // anonymous namespace

int main(int argc, char* argv[]) {
    bool json_output = false;
    bool verbose = false;

    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--json") {
            json_output = true;
        } else if (arg == "-v" || arg == "--verbose") {
            verbose = true;
        } else if (arg == "-h" || arg == "--help") {
            print_help(argv[0]);
            return 0;
        }
    }

    constexpr size_t COMMON_PREFIX_TOKENS = 4096;
    constexpr size_t DIVERGENT_BRANCH_TOKENS = 2048;
    constexpr size_t NUM_STREAMS = 8;
    constexpr size_t PAGE_SIZE_BYTES = hydrogen::PAGE_BLOCK_TOTAL_BYTES; // 1,288 bytes

    // Expected pages:
    // Common root: 4096 / 16 = 256 pages
    // Each leaf:   2048 / 16 = 128 pages
    // Total:       256 + 8 * 128 = 1,280 pages
    // Naive:       8 * (256 + 128) = 3,072 pages
    constexpr size_t EXPECTED_ROOT_PAGES = 256;
    constexpr size_t EXPECTED_LEAF_PAGES = 128;
    constexpr size_t EXPECTED_TOTAL_PAGES = EXPECTED_ROOT_PAGES + (NUM_STREAMS * EXPECTED_LEAF_PAGES); // 1280
    constexpr size_t NAIVE_TOTAL_PAGES = NUM_STREAMS * (EXPECTED_ROOT_PAGES + EXPECTED_LEAF_PAGES);    // 3072

    hydrogen::RadixTree tree;

    // 1. Create common system prompt / tool schema prefix (4,096 tokens)
    std::vector<int32_t> root_prefix(COMMON_PREFIX_TOKENS);
    for (size_t i = 0; i < COMMON_PREFIX_TOKENS; ++i) {
        root_prefix[i] = static_cast<int32_t>(1000 + (i % 500));
    }

    auto root_node = tree.insert(root_prefix);
    assert(root_node != nullptr);
    assert(root_node->page_ids.size() == EXPECTED_ROOT_PAGES);

    // 2. Fork 8 streams diverging from the common prefix concurrently
    std::vector<std::vector<int32_t>> stream_tokens(NUM_STREAMS);
    for (size_t s = 0; s < NUM_STREAMS; ++s) {
        stream_tokens[s].resize(DIVERGENT_BRANCH_TOKENS);
        for (size_t i = 0; i < DIVERGENT_BRANCH_TOKENS; ++i) {
            stream_tokens[s][i] = static_cast<int32_t>(20000 + (s * 5000) + i);
        }
    }

    std::vector<std::shared_ptr<hydrogen::RadixTreeNode>> branch_nodes(NUM_STREAMS);
    std::vector<std::thread> workers;
    workers.reserve(NUM_STREAMS);

    for (size_t s = 0; s < NUM_STREAMS; ++s) {
        workers.emplace_back([&tree, &root_prefix, &stream_tokens, &branch_nodes, s]() {
            // Each stream matches longest common prefix
            auto match = tree.match_longest_prefix(root_prefix);
            assert(match.matched_tokens == COMMON_PREFIX_TOKENS);
            assert(match.physical_page_ids.size() == EXPECTED_ROOT_PAGES);

            // Fork private divergent tokens under the matched node
            branch_nodes[s] = tree.fork_branch(match.last_node, stream_tokens[s]);
        });
    }

    for (auto& w : workers) {
        w.join();
    }

    // 3. Invariant Verifications
    bool root_ref_passed = (root_node->get_ref_count() == NUM_STREAMS);

    // Verify all 256 root page blocks have ref_count == 8
    bool root_pages_ref_passed = true;
    for (uint32_t pid : root_node->page_ids) {
        if (tree.get_page_ref_count(pid) != NUM_STREAMS) {
            root_pages_ref_passed = false;
            break;
        }
    }

    // Verify leaf page blocks have ref_count == 1
    bool leaf_pages_ref_passed = true;
    for (size_t s = 0; s < NUM_STREAMS; ++s) {
        assert(branch_nodes[s] != nullptr);
        if (branch_nodes[s]->page_ids.size() != EXPECTED_LEAF_PAGES) {
            leaf_pages_ref_passed = false;
        }
        for (uint32_t pid : branch_nodes[s]->page_ids) {
            if (tree.get_page_ref_count(pid) != 1) {
                leaf_pages_ref_passed = false;
                break;
            }
        }
    }

    size_t actual_allocated_pages = tree.total_allocated_pages();
    bool page_count_passed = (actual_allocated_pages == EXPECTED_TOTAL_PAGES);

    double page_savings_ratio = static_cast<double>(NAIVE_TOTAL_PAGES - actual_allocated_pages) /
                                static_cast<double>(NAIVE_TOTAL_PAGES);
    bool savings_passed = std::abs(page_savings_ratio - 0.5833333333333334) < 0.001;

    size_t actual_vram_bytes = actual_allocated_pages * PAGE_SIZE_BYTES;
    size_t naive_vram_bytes = NAIVE_TOTAL_PAGES * PAGE_SIZE_BYTES;
    size_t saved_vram_bytes = naive_vram_bytes - actual_vram_bytes;

    bool all_passed = root_ref_passed && root_pages_ref_passed && leaf_pages_ref_passed &&
                      page_count_passed && savings_passed;

    if (json_output) {
        std::cout << "{\n"
                  << "  \"status\": \"" << (all_passed ? "PASS" : "FAIL") << "\",\n"
                  << "  \"root_prefix_tokens\": " << COMMON_PREFIX_TOKENS << ",\n"
                  << "  \"branch_divergent_tokens\": " << DIVERGENT_BRANCH_TOKENS << ",\n"
                  << "  \"num_concurrent_streams\": " << NUM_STREAMS << ",\n"
                  << "  \"root_node_ref_count\": " << root_node->get_ref_count() << ",\n"
                  << "  \"expected_root_ref_count\": " << NUM_STREAMS << ",\n"
                  << "  \"root_pages_allocated\": " << root_node->page_ids.size() << ",\n"
                  << "  \"total_physical_pages\": " << actual_allocated_pages << ",\n"
                  << "  \"naive_physical_pages\": " << NAIVE_TOTAL_PAGES << ",\n"
                  << "  \"pages_saved\": " << (NAIVE_TOTAL_PAGES - actual_allocated_pages) << ",\n"
                  << "  \"page_savings_ratio\": " << std::fixed << std::setprecision(4) << page_savings_ratio << ",\n"
                  << "  \"vram_physical_bytes\": " << actual_vram_bytes << ",\n"
                  << "  \"vram_naive_bytes\": " << naive_vram_bytes << ",\n"
                  << "  \"vram_saved_bytes\": " << saved_vram_bytes << ",\n"
                  << "  \"prefix_sharing_verified\": " << (root_ref_passed && root_pages_ref_passed ? "true" : "false") << ",\n"
                  << "  \"concurrent_branching_verified\": " << (leaf_pages_ref_passed ? "true" : "false") << "\n"
                  << "}\n";
    } else {
        std::cout << "\n===================================================\n"
                  << "Project Hydrogen: Radix-Tree Paged KV Cache Verification\n"
                  << "===================================================\n"
                  << "Common Prefix (Root):      " << COMMON_PREFIX_TOKENS << " tokens ("
                  << EXPECTED_ROOT_PAGES << " pages @ 16 tok/page)\n"
                  << "Concurrent Streams:        " << NUM_STREAMS << " streams\n"
                  << "Divergent Tokens / Stream: " << DIVERGENT_BRANCH_TOKENS << " tokens ("
                  << EXPECTED_LEAF_PAGES << " pages / stream)\n"
                  << "---------------------------------------------------\n"
                  << "Physical Page Allocation:\n"
                  << "  Shared Root Pages:       " << root_node->page_ids.size() << " pages (ref_count="
                  << root_node->get_ref_count() << ") - "
                  << (root_ref_passed ? "PASS" : "FAIL") << "\n"
                  << "  Total Tree Pages:        " << actual_allocated_pages << " pages (Target: "
                  << EXPECTED_TOTAL_PAGES << ") - "
                  << (page_count_passed ? "PASS" : "FAIL") << "\n"
                  << "  Naive Duplication Pages: " << NAIVE_TOTAL_PAGES << " pages\n"
                  << "  VRAM Savings:            " << std::fixed << std::setprecision(2) << (page_savings_ratio * 100.0)
                  << "% (" << (saved_vram_bytes / 1024) << " KiB saved) - "
                  << (savings_passed ? "PASS" : "FAIL") << "\n"
                  << "---------------------------------------------------\n"
                  << "Concurrency & Refcount Integrity:\n"
                  << "  Root Pages Refcount:     " << (root_pages_ref_passed ? "PASS (All 256 pages ref=8)" : "FAIL") << "\n"
                  << "  Leaf Pages Refcount:     " << (leaf_pages_ref_passed ? "PASS (All 1024 pages ref=1)" : "FAIL") << "\n"
                  << "===================================================\n"
                  << "Overall Status:            " << (all_passed ? "PASS (All Invariants Verified)" : "FAIL") << "\n\n";
    }

    return all_passed ? 0 : 1;
}
