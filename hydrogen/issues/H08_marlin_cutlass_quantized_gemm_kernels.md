# Issue Plan: H08 — Marlin / CUTLASS 4-Bit & 8-Bit Quantized GEMM Pipeline

| Field | Value |
| :--- | :--- |
| Issue ID | H08 |
| Title | `[hydrogen] feat(compute): Marlin and CUTLASS 4-bit/8-bit quantized GEMM pipeline` |
| Phase | Phase 2: Cascade & Kernels |
| Labels | `hydrogen`, `phase-2`, `core` |
| Target Files | `hydrogen/src/model/marlin_gemm.cu`, `hydrogen/include/marlin_gemm.hpp` |

---

## 1. Problem Statement
Autoregressive token generation at small-to-medium batch sizes ($B=1..8$) operates in a memory-bandwidth-bound regime where standard GEMM implementations hit register pressure dead-zones and underutilize Tensor Cores. Marlin solves this by using asynchronous `cp.async` pipeline staging and 4-bit weight layout transformations specifically optimized for batch sizes $1 \le B \le 16$ on NVIDIA SM 8.9.

## 2. Implementation Plan
1. Integrate the Marlin 4-bit GEMM kernel into `hydrogen/src/model/marlin_gemm.cu`.
2. Implement weight repacking routine that transforms standard GGUF `Q4_K` tensors into Marlin-compatible interleave formats during model load.
3. Integrate CUTLASS FP8/INT8 matrix multiplication for MLP projection layers.
4. Bound register allocation via `__launch_bounds__(128, 2)` to guarantee maximum occupancy on Ada Lovelace SM 8.9.

## 3. Verification & Proof Plan
- Write `bench_marlin_gemm.cu`.
- Measure GEMM latency across $M \in \{1, 2, 4, 8\}$ with dimensions $K=4096, N=4096$:
  - Verify execution time is $\le 0.15\text{ ms}$ per GEMM.
  - Verify $>80\%$ of RTX 4070 theoretical memory bandwidth ($504\text{ GB/s}$) is achieved during weight reads.

## 4. Acceptance Criteria
- [ ] Weight repacking completes in $<1.0\text{ s}$ during startup.
- [ ] Marlin GEMM achieves $>1.8\times$ speedup over un-optimized cuBLASLt INT4 baselines.
- [ ] Numerical accuracy matches GGUF dequantized outputs within $10^{-3}$ tolerance.
