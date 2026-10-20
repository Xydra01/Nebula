#pragma once

#include "page_block.hpp"
#include <atomic>
#include <cstdint>
#include <memory>
#include <mutex>
#include <shared_mutex>
#include <span>
#include <unordered_map>
#include <vector>

namespace hydrogen {

/**
 * @brief Node in the Radix Tree representing a sequence of tokens and associated physical page blocks.
 */
struct RadixTreeNode : public std::enable_shared_from_this<RadixTreeNode> {
    uint64_t node_id = 0;
    std::vector<int32_t> tokens;
    std::vector<uint32_t> page_ids;
    std::atomic<uint32_t> ref_count{1};
    std::weak_ptr<RadixTreeNode> parent;
    std::unordered_map<int32_t, std::shared_ptr<RadixTreeNode>> children;
    mutable std::shared_mutex mutex;

    RadixTreeNode() = default;
    RadixTreeNode(uint64_t id, std::vector<int32_t> t, std::vector<uint32_t> p)
        : node_id(id), tokens(std::move(t)), page_ids(std::move(p)), ref_count(1) {}

    [[nodiscard]] uint32_t get_ref_count() const noexcept {
        return ref_count.load(std::memory_order_acquire);
    }

    void inc_ref() noexcept {
        ref_count.fetch_add(1, std::memory_order_acq_rel);
    }

    uint32_t dec_ref() noexcept {
        return ref_count.fetch_sub(1, std::memory_order_acq_rel);
    }
};

/**
 * @brief Result of matching a token sequence against the Radix Tree.
 */
struct PrefixMatchResult {
    std::shared_ptr<RadixTreeNode> last_node;
    size_t matched_tokens = 0;
    std::vector<uint32_t> physical_page_ids;
};

/**
 * @brief Thread-safe Radix Tree KV Cache Manager with Copy-on-Write branching.
 */
class RadixTree {
public:
    RadixTree();
    ~RadixTree() = default;

    // Non-copyable, movable
    RadixTree(const RadixTree&) = delete;
    RadixTree& operator=(const RadixTree&) = delete;
    RadixTree(RadixTree&&) noexcept = default;
    RadixTree& operator=(RadixTree&&) noexcept = default;

    /**
     * @brief Match the longest existing token prefix in the tree.
     * Lock-free/read-lock on nodes for high-throughput concurrent inference.
     */
    [[nodiscard]] PrefixMatchResult match_longest_prefix(std::span<const int32_t> tokens) const;

    /**
     * @brief Insert a full token sequence into the tree, allocating new physical pages as needed.
     */
    std::shared_ptr<RadixTreeNode> insert(std::span<const int32_t> tokens);

    /**
     * @brief Fork a child branch from a base node with private divergent tokens (Copy-on-Write).
     * Automatically increments reference counts on shared ancestor pages and allocates new pages for divergent tokens.
     */
    std::shared_ptr<RadixTreeNode> fork_branch(
        const std::shared_ptr<RadixTreeNode>& parent_node,
        std::span<const int32_t> branch_tokens
    );

    /**
     * @brief Increment reference count on a node and all of its ancestor nodes and pages.
     */
    void retain_branch(const std::shared_ptr<RadixTreeNode>& node);

    /**
     * @brief Decrement reference count on a branch; reclaims unreferenced physical pages.
     */
    void release_branch(std::shared_ptr<RadixTreeNode> node);

    /**
     * @brief Total number of physical page blocks currently allocated.
     */
    [[nodiscard]] size_t total_allocated_pages() const noexcept;

    /**
     * @brief Total number of nodes in the radix tree.
     */
    [[nodiscard]] size_t total_nodes() const noexcept;

    /**
     * @brief Retrieve reference count of a specific physical page ID.
     */
    [[nodiscard]] uint32_t get_page_ref_count(uint32_t page_id) const;

    /**
     * @brief Access the root node directly.
     */
    [[nodiscard]] std::shared_ptr<RadixTreeNode> get_root() const noexcept { return root_; }

private:
    std::shared_ptr<RadixTreeNode> root_;
    mutable std::shared_mutex tree_mutex_;
    std::atomic<uint64_t> next_node_id_{1};
    std::atomic<uint32_t> next_page_id_{1};
    std::atomic<size_t> total_allocated_pages_{0};
    std::atomic<size_t> total_nodes_{1};

    // Tracks reference count for each physical page block
    mutable std::mutex page_mutex_;
    std::unordered_map<uint32_t, uint32_t> page_ref_counts_;

    std::vector<uint32_t> allocate_pages_for_tokens(size_t token_count);
    void free_page(uint32_t page_id);
};

} // namespace hydrogen
