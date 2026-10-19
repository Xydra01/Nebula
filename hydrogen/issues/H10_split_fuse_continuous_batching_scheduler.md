# Issue Plan: H10 — Split-Fuse Continuous Batching Scheduler

| Field | Value |
| :--- | :--- |
| Issue ID | H10 |
| Title | `[hydrogen] feat(batcher): Split-Fuse continuous batching scheduler with 64-token prefill chunking` |
| Phase | Phase 3: Batching & Tool Grammar |
| Labels | `hydrogen`, `phase-3`, `core` |
| Target Files | `hydrogen/include/batcher.hpp`, `hydrogen/src/batcher/split_fuse.cpp` |

---

## 1. Problem Statement
When an agent stream receives a tool observation (e.g. 1,500 tokens from a file read), processing the entire prefill at once spikes the batch compute time to $\sim 228\text{ ms}$, stalling all active decode streams and causing unacceptable UI freeze. To eliminate stutter, Hydrogen must divide incoming prefills into chunk size $M$ such that the prefill compute time does not exceed the decode memory read time ($14.08\text{ ms}$).

## 2. Implementation Plan
1. Calculate upper bound prefill chunk size:
   $$M_{\text{max}} = \frac{0.01408 \times 118.0 \times 10^{12}}{18.0 \times 10^9} \approx 92.3\text{ tokens}$$
2. Implement **64-token prefill clamping**: incoming prefills are chunked into $M=64$ slices ($t_{\text{compute}} = 9.76\text{ ms} < 14.08\text{ ms}$).
3. Implement `SplitFuseBatcher` in `hydrogen/src/batcher/split_fuse.cpp`: assemble continuous batches combining $B$ active decode requests with at most 64 tokens from the prefill queue.
4. Pre-allocated attention workspace buffers to prevent dynamic CUDA memory allocations in the batching hot loop.

## 3. Verification & Proof Plan
- Write `bench_split_fuse_batcher.cpp`.
- Simulate 8 active decode streams running continuous token generation.
- Inject a 1,500-token prefill on stream 9:
  - Measure per-token step latency of the 8 decode streams before, during, and after the prefill.
  - Verify decode step latency remains strictly $\le 15.5\text{ ms}$ (less than $10\%$ jitter).
  - Verify the 1,500-token prefill completes in $<300\text{ ms}$ across 24 iterations.

## 4. Acceptance Criteria
- [ ] Zero perceptible decode stutter during massive prefill ingestion.
- [ ] Maximum prefill chunk strictly clamped to 64 tokens.
- [ ] Continuous batch assembly overhead $<15\ \mu\text{s}$ per forward step.
