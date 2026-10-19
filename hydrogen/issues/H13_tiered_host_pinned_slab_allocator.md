# Issue Plan: H13 — Tiered Host Pinned Slab Allocator & 8 Sub-Arenas

| Field | Value |
| :--- | :--- |
| Issue ID | H13 |
| Title | `[hydrogen] feat(memory): Tiered host pinned slab allocator with 8 sub-arenas and cache-line alignment` |
| Phase | Phase 4: DMA Swapping & WDDM |
| Labels | `hydrogen`, `phase-4`, `core` |
| Target Files | `hydrogen/include/host_slab.hpp`, `hydrogen/src/memory/host_slab.cpp` |

---

## 1. Problem Statement
On a Windows workstation running for weeks without reboots, kernel memory fragmentation causes a single contiguous 4 GiB pinned allocation (`cudaHostAlloc`) to fail with `cudaErrorMemoryAllocation` if physically contiguous RAM is unavailable. Furthermore, under 32-probe branching concurrency, multiple threads hammering the same atomic bitmask words cause CPU L3 cache-line false sharing (cache bouncing) and bus-lock spin delays.

## 2. Implementation Plan
1. Implement tiered fallback ladder in `host_slab.cpp`:
   - Tier 1: Single 4 GiB contiguous pinned block (`1 x 4 GB`).
   - Tier 2: Array of four 1 GiB pinned slabs (`4 x 1 GB`).
   - Tier 3: Array of sixteen 256 MiB slabs (`16 x 256 MB`).
2. Implement uniform slot addressing via two-level pointer index (`slab_idx = slot_id / SLOTS_PER_SLAB`, `offset = slot_id % SLOTS_PER_SLAB`) compiled via single-cycle reciprocal multiplication (`imul`).
3. Partition each 256 MB slab into **8 independent sub-arenas** ($26,214$ slots each) aligned to 64-byte hardware cache lines (`alignas(64)`).
4. Equip worker threads with 64-slot thread-local free-list caches, reducing atomic bus transactions by $98.4\%$.
5. Strictly cap total pinned allocation to 4 GiB (`max_pinned_bytes = 4_294_967_296`), preserving at least 23.5 GB of uncommitted RAM on 32 GB systems.

## 3. Verification & Proof Plan
- Write `bench_host_slab.cpp`.
- Simulate fragmented host environment (forcing Tier 2 or Tier 3 allocation):
  - Verify address translation latency is strictly $<1.0\text{ ns/op}$.
  - Spawn 32 worker threads repeatedly allocating and freeing slot blocks:
    - Verify atomic bus transaction reduction exceeds $98\%$.
    - Verify allocation latency remains $<5\text{ ns/op}$ with zero L3 cache line bouncing.

## 4. Acceptance Criteria
- [ ] Tiered fallback successfully initializes under simulated memory fragmentation.
- [ ] Address translation executes in $<1.0\text{ ns}$ using compile-time constants.
- [ ] Zero false sharing or atomic bus contention across 32 concurrent threads.
