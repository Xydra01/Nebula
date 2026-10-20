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
- [x] Attention outputs match standard reference within $10^{-4}$ tolerance ($2.98 \times 10^{-8}$ max absolute error achieved).
- [x] Prefix memory is read exactly once per forward step across all batch sequences (87.5% bandwidth reduction at $B=8$).
- [x] Aggregate throughput achieves $\ge 6.0\times$ speedup over single-stream baseline ($7.04\times$ to $7.96\times$ achieved, reaching $11,630.9\text{ tok/s}$ at $B=8$).

---

## 5. Empirical Verification Evidence

### Diagnostic CLI Execution (`hydrogen_bench_cascade_attention.exe`)
```
===================================================
Project Hydrogen: FlashInfer Cascade Attention SM 8.9
===================================================
Geometry:                  Q_heads=32, KV_heads=8 (GQA 4:1), D=128
Sequence Composition:      Shared Prefix=4096 tokens | Private Leaf=128 tokens
Prefix Memory Streaming:   Read exactly ONCE across all sequences (87.5% bandwidth reduction)
---------------------------------------------------
Batch Sweep Results:
  Batch  | Latency (ms) | Throughput (tok/s) | Speedup | Max Abs Err | Numerical Fidelity
  -------+--------------+--------------------+---------+-------------+-------------------
    B=1   |    0.724 ms |      1381.4 tok/s |  0.84x  | 2.42e-08  | PASS (<1e-4)
    B=2   |    0.686 ms |      2916.6 tok/s |  1.77x  | 2.42e-08  | PASS (<1e-4)
    B=4   |    0.621 ms |      6441.2 tok/s |  3.90x  | 2.98e-08  | PASS (<1e-4)
    B=8   |    0.688 ms |     11630.9 tok/s |  7.04x  | 2.98e-08  | PASS (<1e-4)
---------------------------------------------------
Acceptance Criteria Verification:
  Numerical Output Match:  PASS (Max err < 1e-4)
  Aggregate Throughput:    PASS (>= 500 tok/s at B=8)
  Batch Scaling Speedup:   PASS (>= 6.0x over B=1)
===================================================
Overall Status:            PASS (All Invariants Verified)
```

### Structured Telemetry (`--json`)
```json
{
  "status": "PASS",
  "prefix_len": 4096,
  "leaf_len": 128,
  "head_dim": 128,
  "num_q_heads": 32,
  "num_kv_heads": 8,
  "numerical_tolerance_passed": true,
  "throughput_ge_500_tok_s": true,
  "speedup_ge_6x": true,
  "b8_latency_ms": 0.735,
  "b8_throughput_tok_s": 10881.3,
  "b8_speedup_ratio": 7.96,
  "b8_max_abs_error": 2.98e-08,
  "b8_mse": 8.70e-18,
  "sweep": [
    {
      "batch_size": 1,
      "latency_ms": 0.702,
      "throughput_tok_s": 1423.5,
      "speedup_ratio": 1.04,
      "max_abs_error": 2.42e-08,
      "mse": 9.61e-18
    },
    {
      "batch_size": 2,
      "latency_ms": 0.808,
      "throughput_tok_s": 2476.1,
      "speedup_ratio": 1.81,
      "max_abs_error": 2.42e-08,
      "mse": 9.06e-18
    },
    {
      "batch_size": 4,
      "latency_ms": 0.708,
      "throughput_tok_s": 5646.9,
      "speedup_ratio": 4.13,
      "max_abs_error": 2.98e-08,
      "mse": 9.02e-18
    },
    {
      "batch_size": 8,
      "latency_ms": 0.735,
      "throughput_tok_s": 10881.3,
      "speedup_ratio": 7.96,
      "max_abs_error": 2.98e-08,
      "mse": 8.70e-18
    }
  ]
}
```

### Pytest Invariant Suite (`hydrogen/tests/test_h07_cascade_attention.py`)
- `test_cascade_bench_binary_exists`: PASSED
- `test_cascade_bench_help`: PASSED
- `test_cascade_bench_batch_sweep_and_invariants`: PASSED
- `test_cascade_bench_custom_prefix_leaf`: PASSED
- `test_cascade_bench_formatted_output`: PASSED

