# Issue Plan: H07 — FlashInfer Cascade Attention SM 8.9 Integration

| Field | Value |
| :--- | :--- |
| Issue ID | H07 |
| Title | `[hydrogen] feat(compute): FlashInfer SM 8.9 Cascade Attention kernel integration` |
| Phase | Phase 2: Cascade & Kernels |
| Labels | `hydrogen`, `phase-2`, `core` |
| Target Files | `hydrogen/src/attention/cascade_attention.cu`, `hydrogen/include/cascade_attention.hpp` |

---

## 1. Problem Statement
In multi-agent decoding with a shared prefix, standard PagedAttention launches independent CUDA threadblocks for each sequence. If 8 agents share a 4,096-token prefix, all 8 threadblocks redundantly stream identical prefix KV pages from VRAM, exceeding the RTX 4070's $36\text{ MB}$ L2 cache and choking the $504\text{ GB/s}$ memory bus. FlashInfer Cascade Attention solves this by evaluating prefix attention once across the batch as a dense GEMM, evaluating leaf attention separately, and merging via online softmax rescaling.

## 2. Implementation Plan
1. Integrate FlashInfer SM 8.9 paged and multi-query prefix kernels into the Hydrogen build tree.
2. Implement **Stage 1 (Shared Prefix Projection)**: Stream prefix KV pages once from VRAM, evaluating attention over the entire batch $B=8$ simultaneously.
3. Implement **Stage 2 (Paged Leaf Projection)**: Evaluate attention over unique private leaf pages.
4. Implement **Log-Sum-Exp (LSE) Rescaling**: Fuse Stage 1 and Stage 2 outputs in GPU SRAM using online softmax rescaling:
   $$m_{\text{total}} = \max(m_{\text{prefix}}, m_{\text{leaf}})$$
   $$O_{\text{total}} = \frac{O_{\text{prefix}} e^{m_{\text{prefix}} - m_{\text{total}}} + O_{\text{leaf}} e^{m_{\text{leaf}} - m_{\text{total}}}}{e^{m_{\text{prefix}} - m_{\text{total}}} + e^{m_{\text{leaf}} - m_{\text{total}}}}$$

## 3. Verification & Proof Plan
- Write `bench_cascade_attention.cu`.
- Compare against baseline FlashAttention-2 across batch sizes $B \in \{1, 2, 4, 8\}$ with a 4K shared prefix:
  - Verify numerical output delta is within floating-point tolerance ($<10^{-4}$).
  - Verify aggregate throughput reaches $\ge 500\text{ tok/s}$ at $B=8$ ($14.08\text{ ms/step}$).

## 4. Acceptance Criteria
- [ ] Attention outputs match standard reference within $10^{-4}$ tolerance.
- [ ] Prefix memory is read exactly once per forward step across all batch sequences.
- [ ] Aggregate throughput achieves $\ge 6.0\times$ speedup over single-stream baseline.
