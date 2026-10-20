# Issue Plan: H03 — Single-Token Forward Smoke Test & VRAM Baseline Telemetry

| Field | Value |
| :--- | :--- |
| Issue ID | H03 |
| Title | `[hydrogen] feat(engine): Single-token forward smoke test and VRAM baseline telemetry` |
| Phase | Phase 0: Foundations |
| Labels | `hydrogen`, `phase-0`, `evals` |
| Target Files | `hydrogen/src/smoke_test.cpp`, `hydrogen/src/model/forward_baseline.cu` |

---

## 1. Problem Statement
Before writing complex custom attention and continuous batching kernels, we need an empirical baseline verifying end-to-end forward pass correctness. The smoke test loads the full model into VRAM, embeds a test prompt ("Hello world"), computes a single forward pass, and checks that output token probabilities match the reference baseline from `llama-server`.

## 2. Implementation Plan
1. Implement token embedding lookup in `hydrogen/src/model/forward_baseline.cu`.
2. Implement naive reference forward loop (attention + MLP blocks using basic cuBLAS/cuBLASLt or reference kernels).
3. Compute output projection (RMSNorm + LM head) to extract top-5 logit candidates.
4. Measure memory footprint using `cudaMemGetInfo` and record baseline execution latency using high-resolution CUDA events (`cudaEventRecord`).

## 3. Verification & Proof Plan
- Execute `hydrogen_smoke_test.exe --prompt "The capital of France is"`.
- Verify output:
  - Top-1 token is `" Paris"`.
  - VRAM allocated for model weights is exactly $5,203.5\text{ MiB} \pm 2\%$.
  - Free VRAM remaining is $\ge 6,500\text{ MiB}$ (ready for dynamic KV pool).
- Cross-check logits against `llama-cli -p "The capital of France is" -n 1 --top-k 5`.

## 4. Acceptance Criteria
- [ ] Top-1 token prediction matches reference output.
- [ ] VRAM allocation conforms to Proof A theoretical model weight footprint.
- [ ] Process exits cleanly with zero resource leaks.
