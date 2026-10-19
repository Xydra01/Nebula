# Issue Plan: H18 — End-to-End Regression & Level 1 Harness² Candidate Evaluation

| Field | Value |
| :--- | :--- |
| Issue ID | H18 |
| Title | `[hydrogen] feat(evals): End-to-end multi-agent regression and Level 1 Harness² candidate evaluation` |
| Phase | Phase 5: Daemon Integration |
| Labels | `hydrogen`, `phase-5`, `evals` |
| Target Files | `bench/hydrogen_harness_eval.py`, `crates/nebula-daemon/tests/hydrogen_e2e.rs` |

---

## 1. Problem Statement
Under `llama.cpp`, running recursive self-improvement evaluations under Google Research Harness² required sequential serial probing: evaluating $2k$ candidate probe pairs (`INV` and `DOM`) took 3–5 minutes, stalling the developer loop. Under Hydrogen, because all candidate probes share the workspace prefix, Hydrogen can execute all 8 candidate probes simultaneously in a single batched forward pass, completing the evaluation in ~35 seconds.

## 2. Implementation Plan
1. Implement `crates/nebula-daemon/tests/hydrogen_e2e.rs`: boot daemon with Hydrogen backend, execute complete multi-turn tool calling task (`fs.read`, `fs.write`, `cargo test`).
2. Implement `bench/hydrogen_harness_eval.py`: harness test generating and evaluating 8 candidate probes in parallel.
3. Verify VRAM footprint remains strictly below $3.5\text{ GB}$ of KV cache during 8-stream parallel execution.
4. Verify execution time of 8 candidate probes completes in strictly $<40\text{ seconds}$.

## 3. Verification & Proof Plan
- Execute `uv run pytest bench/test_hydrogen_eval.py`.
- Run full repository test suite:
  - `cargo nextest run --workspace --locked`
- Verify performance benchmarks:
  - 8-stream probe evaluation completes in $<40\text{ s}$ ($6.7\times$ speedup over baseline).
  - 100% of tasks pass verification with zero memory leaks.

## 4. Acceptance Criteria
- [ ] End-to-end multi-turn agent tasks run cleanly through the Nebula daemon.
- [ ] 8-probe Harness² candidate evaluation completes in $<40\text{ seconds}$.
- [ ] Zero regressions across existing Phase 0 and Phase 1 test suites.
