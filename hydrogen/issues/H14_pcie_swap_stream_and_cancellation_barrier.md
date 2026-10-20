# Issue Plan: H14 — PCIe 4.0 DMA Swap Stream & Three-Phase Cancellation Barrier

| Field | Value |
| :--- | :--- |
| Issue ID | H14 |
| Title | `[hydrogen] feat(memory): Asynchronous PCIe 4.0 DMA swap stream and three-phase cancellation barrier` |
| Phase | Phase 4: DMA Swapping & WDDM |
| Labels | `hydrogen`, `phase-4`, `core` |
| Target Files | `hydrogen/src/memory/dma_swapper.cu`, `hydrogen/include/dma_swapper.hpp` |

---

## 1. Problem Statement
When an agent stream enters `AWAITING_TOOL` (running a 10–30s compiler job), its $160\text{ MB}$ leaf KV cache sits idle, consuming VRAM needed by active decoders. Transferring pages over consumer motherboards (B650/Z790) faces root complex bus contention from concurrent NVMe reads. Furthermore, staggering 160 MB across multiple steps risks memory corruption if a tool completes quickly ($<50\text{ ms}$) while eviction is midway.

## 2. Implementation Plan
1. Allocate an isolated, high-priority CUDA swap stream:
   ```cpp
   cudaStreamCreateWithPriority(&swap_stream, cudaStreamNonBlocking, -1);
   ```
2. Implement asynchronous page transfers using `cudaMemcpyAsync` over PCIe 4.0 x16.
3. Implement an explicit three-phase state machine (`RUNNABLE`, `EVICTING`, `SWAPPED`, `RESTORING`) protected by an atomic cancellation barrier:
   ```cpp
   std::atomic<SwapState> state{SwapState::RUNNABLE};
   std::atomic<bool> cancel_eviction{false};
   ```
4. If a tool finishes while in `EVICTING`, assert `cancel_eviction = true`, synchronize the copy stream (`cudaStreamSynchronize`), and reverse restoration *only* for pages transferred to host RAM.

## 3. Verification & Proof Plan
- Write `bench_dma_swapper.cu`.
- Test 1 (Full Swap): Evacuate 160 MB leaf cache to host RAM and restore:
  - Verify eviction completes in $\le 7.5\text{ ms}$ over PCIe 4.0 x16.
  - Verify restoration completes in $\le 7.5\text{ ms}$.
- Test 2 (Early Completion): Signal tool completion 2 ms into eviction:
  - Verify cancellation barrier aborts remaining transfers cleanly.
  - Verify un-evicted pages remain resident in VRAM and stream returns to `RUNNABLE` in $<1\text{ ms}$ without corruption.

## 4. Acceptance Criteria
- [ ] Bidirectional 160 MB swap latency $\le 7.5\text{ ms}$ on PCIe 4.0 x16.
- [ ] Safe mid-flight cancellation with zero memory leaks or read-after-write hazards.
- [ ] Swap operations run entirely asynchronously without stalling active token generation.
