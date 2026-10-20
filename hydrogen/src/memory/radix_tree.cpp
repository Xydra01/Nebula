// hydrogen/src/memory/radix_tree.cpp
// Radix-tree paged KV cache manager with atomic Copy-on-Write branching

#include "radix_tree.hpp"
#include <algorithm>
#include <cassert>
#include <iostream>

namespace hydrogen {

RadixTree::RadixTree() {
    root_ = std::make_shared<RadixTreeNode>(0, std::vector<int32_t>{}, std::vector<uint32_t>{});
    root_->ref_count.store(0, std::memory_order_relaxed);
    total_nodes_.store(1, std::memory_order_relaxed);
}

std::vector<uint32_t> RadixTree::allocate_pages_for_tokens(size_t token_count) {
    if (token_count == 0) {
        return {};
    }
    size_t num_pages = (token_count + PAGE_BLOCK_TOKENS - 1) / PAGE_BLOCK_TOKENS;
    std::vector<uint32_t> page_ids;
    page_ids.reserve(num_pages);

    std::lock_guard lock(page_mutex_);
    for (size_t i = 0; i < num_pages; ++i) {
        uint32_t pid = next_page_id_.fetch_add(1, std::memory_order_relaxed);
        page_ref_counts_[pid] = 0;
        page_ids.push_back(pid);
    }
    total_allocated_pages_.fetch_add(num_pages, std::memory_order_relaxed);
    return page_ids;
}

void RadixTree::free_page(uint32_t page_id) {
    std::lock_guard lock(page_mutex_);
    auto it = page_ref_counts_.find(page_id);
    if (it != page_ref_counts_.end()) {
        page_ref_counts_.erase(it);
        if (total_allocated_pages_ > 0) {
            total_allocated_pages_.fetch_sub(1, std::memory_order_relaxed);
        }
    }
}

PrefixMatchResult RadixTree::match_longest_prefix(std::span<const int32_t> tokens) const {
    PrefixMatchResult result;
    result.last_node = root_;
    result.matched_tokens = 0;

    if (tokens.empty()) {
        return result;
    }

    std::shared_lock tree_lock(tree_mutex_);
    std::shared_ptr<RadixTreeNode> curr = root_;
    size_t offset = 0;

    while (offset < tokens.size()) {
        std::shared_lock node_lock(curr->mutex);
        int32_t first_tok = tokens[offset];
        auto it = curr->children.find(first_tok);
        if (it == curr->children.end()) {
            break;
        }

        std::shared_ptr<RadixTreeNode> child = it->second;
        std::shared_lock child_lock(child->mutex);

        // Compare child tokens with remaining input tokens
        size_t match_len = 0;
        size_t max_cmp = std::min(child->tokens.size(), tokens.size() - offset);
        while (match_len < max_cmp && child->tokens[match_len] == tokens[offset + match_len]) {
            match_len++;
        }

        if (match_len == 0) {
            break;
        }

        if (match_len == child->tokens.size()) {
            // Full node match, advance into child
            offset += match_len;
            result.physical_page_ids.insert(
                result.physical_page_ids.end(),
                child->page_ids.begin(),
                child->page_ids.end()
            );
            curr = child;
            result.last_node = curr;
            result.matched_tokens = offset;
        } else {
            // Partial node match within child: collect matched pages
            size_t matched_pages = (match_len + PAGE_BLOCK_TOKENS - 1) / PAGE_BLOCK_TOKENS;
            matched_pages = std::min(matched_pages, child->page_ids.size());
            result.physical_page_ids.insert(
                result.physical_page_ids.end(),
                child->page_ids.begin(),
                child->page_ids.begin() + matched_pages
            );
            offset += match_len;
            result.last_node = child;
            result.matched_tokens = offset;
            break;
        }
    }

    return result;
}

std::shared_ptr<RadixTreeNode> RadixTree::insert(std::span<const int32_t> tokens) {
    if (tokens.empty()) {
        return root_;
    }

    std::unique_lock tree_lock(tree_mutex_);
    std::shared_ptr<RadixTreeNode> curr = root_;
    size_t offset = 0;

    while (offset < tokens.size()) {
        std::unique_lock node_lock(curr->mutex);
        int32_t first_tok = tokens[offset];
        auto it = curr->children.find(first_tok);

        if (it == curr->children.end()) {
            // No existing branch starts with first_tok: create new leaf node
            auto rem_tokens = tokens.subspan(offset);
            std::vector<int32_t> node_tokens(rem_tokens.begin(), rem_tokens.end());
            auto pages = allocate_pages_for_tokens(node_tokens.size());
            uint64_t nid = next_node_id_.fetch_add(1, std::memory_order_relaxed);

            auto new_node = std::make_shared<RadixTreeNode>(nid, std::move(node_tokens), std::move(pages));
            new_node->parent = curr;
            new_node->ref_count.store(0, std::memory_order_relaxed); // Template until retained/forked
            curr->children[first_tok] = new_node;
            total_nodes_.fetch_add(1, std::memory_order_relaxed);
            return new_node;
        }

        std::shared_ptr<RadixTreeNode> child = it->second;
        std::unique_lock child_lock(child->mutex);

        // Check common prefix length between child->tokens and remaining tokens
        size_t match_len = 0;
        size_t max_cmp = std::min(child->tokens.size(), tokens.size() - offset);
        while (match_len < max_cmp && child->tokens[match_len] == tokens[offset + match_len]) {
            match_len++;
        }

        if (match_len == child->tokens.size()) {
            // Exact full match of child, traverse downwards
            offset += match_len;
            curr = child;
            continue;
        }

        // Split child edge: child[0..match_len-1] becomes split_node,
        // child becomes child[match_len..end]
        size_t split_page_count = (match_len + PAGE_BLOCK_TOKENS - 1) / PAGE_BLOCK_TOKENS;
        split_page_count = std::min(split_page_count, child->page_ids.size());

        std::vector<uint32_t> split_pages(child->page_ids.begin(), child->page_ids.begin() + split_page_count);
        std::vector<uint32_t> child_rem_pages(child->page_ids.begin() + split_page_count, child->page_ids.end());

        size_t child_rem_tokens = child->tokens.size() - match_len;
        size_t needed_child_pages = (child_rem_tokens + PAGE_BLOCK_TOKENS - 1) / PAGE_BLOCK_TOKENS;
        while (child_rem_pages.size() < needed_child_pages) {
            auto extra = allocate_pages_for_tokens(PAGE_BLOCK_TOKENS);
            child_rem_pages.push_back(extra[0]);
        }

        uint64_t split_id = next_node_id_.fetch_add(1, std::memory_order_relaxed);
        std::vector<int32_t> split_tokens(child->tokens.begin(), child->tokens.begin() + match_len);

        auto split_node = std::make_shared<RadixTreeNode>(split_id, std::move(split_tokens), std::move(split_pages));
        split_node->parent = curr;
        split_node->ref_count.store(child->ref_count.load(std::memory_order_relaxed), std::memory_order_relaxed);

        int32_t child_cont_tok = child->tokens[match_len];
        child->tokens.erase(child->tokens.begin(), child->tokens.begin() + match_len);
        child->page_ids = std::move(child_rem_pages);
        child->parent = split_node;

        split_node->children[child_cont_tok] = child;
        curr->children[first_tok] = split_node;
        total_nodes_.fetch_add(1, std::memory_order_relaxed);

        offset += match_len;
        if (offset == tokens.size()) {
            return split_node;
        }

        // Create new divergent branch off split_node
        auto div_rem = tokens.subspan(offset);
        std::vector<int32_t> div_tokens(div_rem.begin(), div_rem.end());
        auto div_pages = allocate_pages_for_tokens(div_tokens.size());
        uint64_t div_id = next_node_id_.fetch_add(1, std::memory_order_relaxed);

        auto div_node = std::make_shared<RadixTreeNode>(div_id, std::move(div_tokens), std::move(div_pages));
        div_node->parent = split_node;
        div_node->ref_count.store(0, std::memory_order_relaxed);
        split_node->children[tokens[offset]] = div_node;
        total_nodes_.fetch_add(1, std::memory_order_relaxed);
        return div_node;
    }

    return curr;
}

std::shared_ptr<RadixTreeNode> RadixTree::fork_branch(
    const std::shared_ptr<RadixTreeNode>& parent_node,
    std::span<const int32_t> branch_tokens
) {
    if (!parent_node) {
        throw std::invalid_argument("parent_node cannot be null");
    }

    // Increment reference counts on all shared ancestor nodes and pages (Copy-on-Write)
    retain_branch(parent_node);

    if (branch_tokens.empty()) {
        return parent_node;
    }

    std::unique_lock tree_lock(tree_mutex_);
    std::unique_lock parent_lock(parent_node->mutex);

    int32_t first_tok = branch_tokens[0];
    auto it = parent_node->children.find(first_tok);
    if (it != parent_node->children.end()) {
        // Child already exists with this token: return it with incremented ref
        retain_branch(it->second);
        return it->second;
    }

    // Allocate private pages exclusively for the divergent tokens
    std::vector<int32_t> tokens(branch_tokens.begin(), branch_tokens.end());
    auto private_pages = allocate_pages_for_tokens(tokens.size());
    uint64_t nid = next_node_id_.fetch_add(1, std::memory_order_relaxed);

    auto branch_node = std::make_shared<RadixTreeNode>(nid, std::move(tokens), std::move(private_pages));
    branch_node->parent = parent_node;
    branch_node->ref_count.store(1, std::memory_order_relaxed); // 1 active stream owner

    {
        std::lock_guard lock(page_mutex_);
        for (uint32_t pid : branch_node->page_ids) {
            page_ref_counts_[pid] = 1;
        }
    }

    parent_node->children[first_tok] = branch_node;
    total_nodes_.fetch_add(1, std::memory_order_relaxed);

    return branch_node;
}

void RadixTree::retain_branch(const std::shared_ptr<RadixTreeNode>& node) {
    std::shared_ptr<RadixTreeNode> curr = node;
    while (curr && curr != root_) {
        curr->inc_ref();
        {
            std::lock_guard lock(page_mutex_);
            for (uint32_t pid : curr->page_ids) {
                page_ref_counts_[pid]++;
            }
        }
        curr = curr->parent.lock();
    }
}

void RadixTree::release_branch(std::shared_ptr<RadixTreeNode> node) {
    std::shared_ptr<RadixTreeNode> curr = std::move(node);
    while (curr && curr != root_) {
        std::shared_ptr<RadixTreeNode> next_parent = curr->parent.lock();
        uint32_t rem = curr->dec_ref();

        {
            std::lock_guard lock(page_mutex_);
            for (uint32_t pid : curr->page_ids) {
                auto it = page_ref_counts_.find(pid);
                if (it != page_ref_counts_.end()) {
                    if (it->second > 1) {
                        it->second--;
                    } else {
                        page_ref_counts_.erase(it);
                        if (total_allocated_pages_ > 0) {
                            total_allocated_pages_.fetch_sub(1, std::memory_order_relaxed);
                        }
                    }
                }
            }
        }

        // If node ref dropped to 0, prune it from parent's children map
        if (rem == 1 && next_parent) {
            std::unique_lock parent_lock(next_parent->mutex);
            if (!curr->tokens.empty()) {
                next_parent->children.erase(curr->tokens[0]);
            }
        }

        curr = std::move(next_parent);
    }
}

size_t RadixTree::total_allocated_pages() const noexcept {
    return total_allocated_pages_.load(std::memory_order_relaxed);
}

size_t RadixTree::total_nodes() const noexcept {
    return total_nodes_.load(std::memory_order_relaxed);
}

uint32_t RadixTree::get_page_ref_count(uint32_t page_id) const {
    std::lock_guard lock(page_mutex_);
    auto it = page_ref_counts_.find(page_id);
    return (it != page_ref_counts_.end()) ? it->second : 0;
}

} // namespace hydrogen
