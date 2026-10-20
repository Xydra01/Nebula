# Issue Plan: H12 — Speculative Tool Parser & Mutating Turn-Finalization Barrier

| Field | Value |
| :--- | :--- |
| Issue ID | H12 |
| Title | `[hydrogen] feat(tools): Speculative tool argument parser and mutating turn-finalization barrier` |
| Phase | Phase 3: Batching & Tool Grammar |
| Labels | `hydrogen`, `phase-3`, `safety` |
| Target Files | `hydrogen/src/tools/speculative_dispatcher.cpp`, `hydrogen/include/tool_barrier.hpp` |

---

## 1. Problem Statement
Waiting for an LLM to generate closing brackets and whitespace (`\n}`) before dispatching tool I/O wastes 15–20 ms per tool call. However, speculatively executing stateful operations (`fs.write`, `git.commit`, `shell.run`) before token generation finishes is catastrophic: hallucinated or rewritten arguments could permanently corrupt files or launch unauthorized processes. Speculative execution must be strictly restricted to idempotent, read-only tools, with mutating tools blocked behind an in-engine barrier.

## 2. Implementation Plan
1. Implement token-stream parameter extraction in `speculative_dispatcher.cpp`: extract tool name and arguments token-by-token during generation.
2. Trigger early background I/O upon parameter closing quote (`"`): for `fs.read`, begin reading the target file into pinned host RAM while the model emits closing delimiters.
3. Implement `tool_barrier.hpp`: define a strict whitelist of speculative-eligible tools (`fs.read`, `fs.list`, `git.status`, `code.lookup`, `memory.lookup`).
4. For all other tools (mutating operations), enforce an execution barrier: tool invocation is blocked until the turn emits the terminal delimiter, the turn is finalized, and approval is verified.

## 3. Verification & Proof Plan
- Write `test_speculative_safety.cpp`.
- Test case 1 (Read-only tool): Stream emits `{"name": "fs.read", "path": "src/lib.rs" ...`. Verify file read initiates in background at closing quotation mark.
- Test case 2 (Mutating tool): Stream emits `{"name": "fs.write", "path": "test.txt" ...`. Verify file write is strictly blocked until turn completion.
- Test case 3 (Model rewrite/abort): Stream generates partial mutating command, then aborts. Verify zero file modifications or process spawns occurred.

## 4. Acceptance Criteria
- [x] Read-only tools resolve file data before terminal token generation finishes. (Measured: **33.11 ms lead time** before terminal token emission, file resident in pinned host RAM).
- [x] Mutating tools are 100% blocked until turn finalization and executor approval. (Measured: **0 bytes written pre-finalization**, blocked by in-engine barrier).
- [x] Zero premature side-effects under simulated model hallucinations or mid-turn aborts. (Measured: **0 processes spawned, 0 bytes written, 0 premature side effects**).

## 5. Empirical Verification Output

```json
{
  "status": "PASS",
  "acceptance_criteria": {
    "readonly_early_resolution_passed": true,
    "mutating_barrier_100pct_blocked_passed": true,
    "zero_premature_side_effects_passed": true
  },
  "metrics": {
    "ro_lead_time_ms": 33.11,
    "ro_bytes_loaded": 80,
    "mutating_blocked_before_approval": true,
    "mutating_side_effects_before_approval": false,
    "abort_process_spawned": false,
    "premature_side_effects_count": 0
  }
}
```
