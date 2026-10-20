# Project Hydrogen — Phase 1 Implementation Plan: Memory Subsystem & Radix Tree

| Field | Value |
| :--- | :--- |
| Status | Ready to plan |
| Component | Hydrogen Inference Engine |
| Architecture Ref | [`docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md`](../../docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md) (Section 3.1) |
| Dependencies | Phase 0 completed |
| GitHub Tag | `[hydrogen]`, label `hydrogen`, `phase-1` |

---

## 1. Goal

By the end of **Phase 1**:
- Physical GPU VRAM is managed as a uniform, paged pool of 16-token `UniformPageBlock` containers supporting dual FP8 (E4M3) and Q8_0 quantizations.
- A lock-free, zero-dependency C++20 Radix Tree indexes shared conversation prefixes and tool schemas, supporting Copy-on-Write (CoW) branching for multi-agent rollouts.
- When multiple agent streams share a common prompt, identical prefix pages are referenced atomically without data duplication, achieving a verified **$\ge 75\%$ memory reduction**.

---

## 2. In Scope & Out of Scope

### 2.1 In Scope
- **Uniform Page Structure**: `UniformPageBlock` (1,280 bytes per 16 tokens) with 1-byte quantization tag (`0=FP8_E4M3`, `1=Q8_0`) and per-page dynamic dequantization scale.
- **Dual-Quantization Hybrid Policy**: Sensitive attention layers (initial 4 input and final 4 output layers) automatically allocate Q8_0 blocks; intermediate layers allocate FP8 blocks to prevent syntax drift on code.
- **Radix Tree Implementation**: Modern C++20 tree structure with atomic reference counts (`std::atomic<uint32_t> ref_count`), thread-safe branch creation, and fork/split logic.
- **Lock-Free Page Pool Allocator**: Bitmask-backed physical page allocation table managing 16-token page slots in VRAM without OS syscalls.

### 2.2 Out of Scope
- FlashInfer attention computation over these pages (Phase 2).
- Continuous prefill chunking (Phase 3).
- Swapping pages over PCIe to host RAM (Phase 4).

---

## 3. Workstreams & Issues Breakdown

| Issue ID | Title | Summary & Deliverable | Exit Check |
| :--- | :--- | :--- | :--- |
| **H04** | Uniform Page Block & Dual-Quantization Layout | Define `UniformPageBlock`, implement per-channel dynamic scale packing, and allocate physical VRAM page pool. | Unit test verifies 1,280-byte alignment and FP8/Q8_0 tag serialization. |
| **H05** | Radix Tree Node Management & CoW Branching | Implement radix tree prefix insertion, prefix matching, reference count increments, and Copy-on-Write node splitting. | Unit test simulates 8 divergent branches; verifies zero duplicate prefix allocations. |
| **H06** | Lock-Free Free-List Allocator & VRAM Compaction | Bitmask-based page allocator managing physical VRAM blocks with background garbage collection and defragmentation sweep. | Stress test runs 50,000 branch fork/prune cycles with zero page leaks. |

---

## 4. Phase 1 Exit Criteria

Phase 1 is considered **finished** only when all of the following verifiable conditions are met:

1. **Prefix Sharing Proof**: Simulating 8 agent branches diverging from a 4,096-token shared prefix requires strictly $\le 1,600\text{ MiB}$ of physical KV cache in VRAM (matching Proof A calculation of $1,600.0\text{ MiB}$ vs naive $7,680.0\text{ MiB}$).
2. **Allocation Speed**: Page allocation and CoW branch splitting take strictly $<50\ \mu\text{s}$ per 16-token page block.
3. **Reference Count Invariants**: Branch pruning immediately frees unreferenced leaf pages back to the free-list without mutating or invalidating shared ancestor nodes.
4. **Stress & Hygiene**: Zero memory leaks or dangling page pointer faults after a 10-minute continuous random branching/pruning fuzz test.
