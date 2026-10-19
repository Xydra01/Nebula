# Project Hydrogen — Phase 2 Implementation Plan: Cascade Attention & Compute Kernels

| Field | Value |
| :--- | :--- |
| Status | Ready to plan |
| Component | Hydrogen Inference Engine |
| Architecture Ref | [`docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md`](../../docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md) (Sections 2, 3.2) |
| Dependencies | Phase 1 completed |
| GitHub Tag | `[hydrogen]`, label `hydrogen`, `phase-2` |

---

## 1. Goal

By the end of **Phase 2**:
- The FlashInfer attention library is integrated and tuned for Ada Lovelace SM 8.9 (`compute_89`), implementing **Cascade Attention** over the Radix-Tree KV cache.
- Attention over the shared 4K-token system prompt prefix is computed **once** across the batch in a dense GEMM, with leaf attention evaluated separately and merged via online softmax rescaling.
- Marlin / CUTLASS quantized 4-bit GEMM kernels are bound to the weight forward pass, bypassing the $B=4..16$ register pressure bottleneck.
- Aggregate system throughput across 8 parallel streams reaches **$\ge 500\text{ tokens/s}$** with individual kernel runtimes strictly clamped below $5.0\text{ ms}$ to eliminate Windows WDDM preemption jitter.

---

## 2. In Scope & Out of Scope

### 2.1 In Scope
- **FlashInfer Cascade Attention**: Stage 1 shared prefix projection + Stage 2 paged leaf attention + online softmax rescaling (`merge_states`).
- **Marlin / CUTLASS 4-Bit Matrix Multiplication**: Asynchronous `cp.async` pipeline for quantized weights.
- **Templated Kernel Dispatch**: Specialized instantiations for `cascade_attention_kernel<QuantType::FP8>` and `<QuantType::Q8_0>` eliminating warp divergence on mixed quantization pages.
- **Binary Footprint Ceiling**: Constraining template expansions strictly to target 9B GQA geometry ($d_{\text{head}}=128$, GQA 4:1) ensuring compiled `.cubin` footprint remains $<15.0\text{ MB}$.
- **WDDM Kernel Clamping**: Tiling GEMMs to keep individual kernel execution times strictly under $5.0\text{ ms}$.

### 2.2 Out of Scope
- Continuous mixed prefill/decode batching (Phase 3).
- Speculative tool execution (Phase 3).
- Host memory swapping (Phase 4).

---

## 3. Workstreams & Issues Breakdown

| Issue ID | Title | Summary & Deliverable | Exit Check |
| :--- | :--- | :--- | :--- |
| **H07** | FlashInfer Cascade Attention SM 8.9 Integration | Compile FlashInfer SM 8.9 kernels, implement Stage 1 shared prefix GEMM and Stage 2 leaf paged attention with online softmax merging. | Unit benchmark verifies numerical equivalence with standard FlashAttention-2 within $10^{-4}$ LSE tolerance. |
| **H08** | Marlin / CUTLASS 4-Bit GEMM Pipeline | Integrate Marlin 4-bit GEMMs with asynchronous `cp.async` staging for $B=1..8$ batch sizes. | Matrix multiplication benchmark demonstrates $>80\%$ of theoretical Tensor Core throughput. |
| **H09** | Specialized Templated Dispatch & WDDM Clamping | Implement batch-level sorting by `quant_type` and enforce $<5.0\text{ ms}$ execution ceiling per kernel launch. | Verification under Nsight Systems proves no warp divergence and max kernel time $<5.0\text{ ms}$. |

---

## 4. Phase 2 Exit Criteria

Phase 2 is considered **finished** only when all of the following verifiable conditions are met:

1. **Throughput Scaling**: With $B=8$ streams sharing a 4,096-token prefix, aggregate decoding throughput reaches strictly $\ge 500\text{ tokens/s}$ on the RTX 4070 (verifying the theoretical calculation of $568.2\text{ tok/s}$ in Proof B).
2. **Individual Stream Latency**: Per-stream token latency at $B=8$ remains strictly $\le 16.0\text{ ms/token}$ (less than 20% slowdown vs single-stream $11.8\text{ ms}$).
3. **WDDM Preemption Compliance**: 100% of individual kernel dispatches complete in $<5.0\text{ ms}$ under synthetic GPU load.
4. **Binary Footprint**: The compiled CUDA shared library / binary is strictly $<15.0\text{ MB}$ with sub-200ms driver initialization latency.
