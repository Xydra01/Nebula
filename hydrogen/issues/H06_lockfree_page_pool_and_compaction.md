# Issue Plan: H06 — Lock-Free Page Pool Allocator & VRAM Compaction Sweeper

| Field | Value |
| :--- | :--- |
| Issue ID | H06 |
| Title | `[hydrogen] feat(memory): Lock-free page pool allocator and background VRAM compaction sweeper` |
| Phase | Phase 1: Memory Subsystem |
| Labels | `hydrogen`, `phase-1`, `core` |
| Target Files | `hydrogen/include/page_pool.hpp`, `hydrogen/src/memory/page_pool.cu` |

---

## 1. Problem Statement
Rapidly spawning and terminating dozens of short-lived probe branches in recursive self-improvement causes page fragmentation in physical VRAM. Standard heap allocators introduce mutex contention and OS context-switch overheads in the hot forward loop. Hydrogen requires a pre-allocated physical page pool managed by atomic bitmasks and a background defragmentation sweeper.

## 2. Implementation Plan
1. Allocate fixed physical VRAM page pool: $6,604.5\text{ MiB}$ partitioned into uniform 1,280-byte blocks ($5,410,200$ page blocks).
2. Manage free pages using a 64-bit atomic bitmask array (`std::atomic<uint64_t> page_bitmap[]`).
3. Implement `allocate_page()` and `free_page()` using non-blocking bit scan intrinsics (`_tzcnt_u64` / `__builtin_ctzll`).
4. Implement a background garbage collector that runs during GPU idle steps to compact physical page tables and defragment disparate leaf sequences.

## 3. Verification & Proof Plan
- Write `stress_test_page_pool.cpp`.
- Spawn 16 concurrent threads repeatedly allocating and freeing random batches of 1 to 64 pages across 500,000 operations.
- Verify:
  - Allocation time strictly $<50\text{ ns/page}$.
  - Zero double-free or page leakage errors.
  - VRAM utilization matches exact page accounting.

## 4. Acceptance Criteria
- [ ] Bitmask allocator operates lock-free across concurrent worker threads.
- [ ] 100% of freed pages return cleanly to the available pool.
- [ ] Stress test completes with zero memory leaks and zero allocation stalls.
