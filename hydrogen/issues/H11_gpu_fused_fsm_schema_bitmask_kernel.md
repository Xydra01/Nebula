# Issue Plan: H11 — GPU-Fused FSM Tool Grammar Bitmask Sampler

| Field | Value |
| :--- | :--- |
| Issue ID | H11 |
| Title | `[hydrogen] feat(grammar): GPU-fused FSM bitmask sampler with on-chip SRAM validation` |
| Phase | Phase 3: Batching & Tool Grammar |
| Labels | `hydrogen`, `phase-3`, `core` |
| Target Files | `hydrogen/src/grammar/fsm_compiler.cpp`, `hydrogen/src/grammar/fsm_sampler.cu` |

---

## 1. Problem Statement
Traditional grammar-constrained sampling (e.g. GBNF in `llama.cpp`) parses pushdown automata on the CPU. On every single token, the GPU must pause, synchronize over PCIe, wait for CPU regex parsing, and copy allowed token masks back to the GPU. This introduces a **$2.4\text{ ms}$ stall per token**, reducing token throughput by 20–30%.

## 2. Implementation Plan
1. Implement `fsm_compiler.cpp`: pre-compile registered JSON tool schemas during initialization into deterministic Finite State Machine (FSM) tables.
2. Encode allowed next-token transitions as pre-computed bitmasks:
   $$\text{Mask Size} = \left\lceil \frac{152,064}{32} \right\rceil = 4,752\text{ uint32\_t words} \approx 19.0\text{ KiB}$$
3. Implement `fsm_sampler.cu`: stage active state bitmask directly into GPU Shared Memory (SRAM) and apply in-place logit masking:
   $$\text{logits}[i] = (\text{mask}[i \gg 5] \ \& \ (1 \ll (i \ \& \ 31))) \ ?\ \text{logits}[i] : -\infty$$
4. Execute directly within the top-p/temperature sampling kernel without CPU round-trips.

## 3. Verification & Proof Plan
- Write `bench_fsm_sampler.cu`.
- Compare sampling latency between CPU GBNF regex parsing and Hydrogen GPU-fused bitmask sampler:
  - Verify GPU bitmask execution latency is $\le 2.0\ \mu\text{s}$ (a $1,200\times$ speedup over CPU 2.4 ms).
  - Generate 10,000 tool calls and validate that 100% of generated outputs parse cleanly with `nlohmann::json` or `serde_json` with zero syntax errors.

## 4. Acceptance Criteria
- [ ] Schema validation executes in $<2.0\ \mu\text{s}$ per token on the GPU.
- [ ] 0 CPU-GPU stream synchronizations during tool argument generation.
- [ ] 100% JSON schema conformity across synthetic and real tool invocations.
