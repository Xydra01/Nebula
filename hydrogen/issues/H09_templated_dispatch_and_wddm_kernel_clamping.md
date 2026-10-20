# Issue Plan: H09 — Templated Dispatch & WDDM Sub-5ms Kernel Execution Clamping

| Field | Value |
| :--- | :--- |
| Issue ID | H09 |
| Title | `[hydrogen] feat(compute): Templated kernel dispatch and sub-5ms WDDM quantum execution clamping` |
| Phase | Phase 2: Cascade & Kernels |
| Labels | `hydrogen`, `phase-2`, `safety` |
| Target Files | `hydrogen/src/attention/dispatch.cpp`, `hydrogen/src/model/kernel_tiler.cu` |

---

## 1. Problem Statement
Maintaining mixed FP8 and Q8_0 pages in the same Radix tree could force inner warp GEMM loops to branch across quantization formats, causing warp divergence and cutting Tensor Core throughput in half. Furthermore, combinatorially instantiating kernels across arbitrary head dimensions inflates `.cubin` binary sizes beyond 100 MB. Finally, Windows WDDM preempts any compute kernel exceeding its scheduling quantum ($\sim 15\text{ ms}$), causing 15–40 ms latency spikes that destroy interactive agent responsiveness.

## 2. Implementation Plan
1. Implement forward scheduler pre-sorting: active attention requests are sorted by `quant_type` during batch assembly.
2. Dispatch to specialized templated kernels:
   ```cpp
   if (page_block.quant_type == QuantType::FP8) {
       cascade_attention_kernel<QuantType::FP8><<<grid, block, shmem, stream>>>(...);
   } else if (page_block.quant_type == QuantType::Q8_0) {
       cascade_attention_kernel<QuantType::Q8_0><<<grid, block, shmem, stream>>>(...);
   }
   ```
3. Cap template expansions strictly to target 9B geometry ($d_{\text{head}}=128$, GQA 4:1) ensuring compiled CUDA binary size remains $<15.0\text{ MB}$.
4. Clamp all kernel dispatches strictly under **$5.0\text{ ms}$** via micro-chunked tile sizing, ensuring clean GPU yielding at OS scheduling boundaries without WDDM preemption penalties.

## 3. Verification & Proof Plan
- Profile dispatch loop with NVIDIA Nsight Compute:
  - Verify warp execution efficiency $>95\%$ (zero warp divergence in attention loop).
  - Verify maximum individual kernel execution duration across 10,000 steps is strictly $<5.0\text{ ms}$.
  - Verify compiled `.cubin` binary size is $<15.0\text{ MB}$.

## 4. Acceptance Criteria
- [ ] No branching inside inner warp GEMM loops.
- [ ] Compiled binary size $<15.0\text{ MB}$ with sub-200ms driver load time.
- [ ] Zero WDDM preemption stalls or desktop compositor stutter during continuous batch decoding.
