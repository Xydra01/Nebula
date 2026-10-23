# Issue Plan: H23 — End-to-End Conversational Chat & Parallel Agent Task Evaluation

| Field | Value |
| :--- | :--- |
| Issue ID | H23 |
| Title | `[hydrogen] feat(evals): End-to-end interactive conversational chat, multi-turn task completion, and 8-stream parallel evaluation` |
| Phase | Phase 6: Native Engine & Autonomous Agent Execution |
| Labels | `hydrogen`, `phase-6`, `evals` |
| Target Files | `crates/nebula-daemon/tests/hydrogen_live_e2e.rs`, `bench/hydrogen_harness_eval.py`, `crates/nebula-cli/src/chat.rs`, `crates/nebula-daemon/src/executor.rs` |

---

## 1. Problem Statement

Previous integration tests (`crates/nebula-daemon/tests/hydrogen_e2e.rs`) relied on an in-process `MockEngine` simulating shared memory IPC responses rather than running real GGUF weights on the compiled `hydrogen_engine.exe`. Phase 6 is complete only when the live native engine powers normal interactive chat (`nebula chat`), completes multi-turn autonomous coding tasks with real tools (`nebula task`), and runs 8 parallel candidate probe rollouts under Harness² in strictly $< 40\text{ seconds}$ with zero regressions.

## 2. Implementation Plan

1. **Live Conversational Chat Validation (`nebula chat`)**:
   - Set `[model.profiles.hydrogen]` in `config/default.toml` with `backend = "hydrogen"` and `runtime = "hydrogen"`.
   - Boot daemon: `nebula daemon start`.
   - Execute interactive multi-turn session via CLI:
     - User asks programming questions; verify natural streaming output ($< 15\text{ ms}$ inter-token latency).
     - User gives follow-up commands; verify previous turns are retained in ChatML context.
2. **Autonomous Multi-Turn Agent Task Validation (`nebula task`)**:
   - Create a task in an isolated git worktree:
     ```powershell
     nebula task create --goal "Inspect src/lib.rs, fix the broken assertion, and run cargo test"
     ```
   - Verify execution flow:
     - Model generates tool call `fs.read {"path": "src/lib.rs"}` over shared memory IPC.
     - Daemon executor runs tool in Windows Job Object sandbox and returns file contents.
     - Model ingests observation, writes patch with `fs.write`, and runs tests with `shell.run`.
     - Model outputs task completion summary; executor marks task `Completed`.
3. **8-Stream Parallel Candidate Evaluation (Harness²)**:
   - In `bench/hydrogen_harness_eval.py`, spawn 8 concurrent agent tasks simultaneously under Level 1 task recursion.
   - Verify aggregate throughput exceeds $500\text{ tokens/s}$.
   - Verify entire 8-probe evaluation finishes in strictly $< 40\text{ seconds}$ without memory leaks or OOM.
4. **Full Workspace Regression Suite**:
   - Execute CI suite: `cargo nextest run --workspace --locked`.
   - Execute bench suite: `cd bench; uv run pytest -q`.

## 3. Verification & Proof Plan

- Run live integration test: `cargo nextest run -p nebula-daemon --test hydrogen_live_e2e --locked`.
- Run Python Harness² probe evaluation: `cd bench; uv run pytest test_hydrogen_parallel.py`.
- Verify CPU and memory counters:
  - VRAM KV cache stays $< 3.5\text{ GB}$.
  - System commit headroom stays $> 23.5\text{ GB}$.
  - `active_tool_processes` returns cleanly to 0 upon completion.

## 4. Acceptance Criteria

- [ ] `nebula chat` holds fluent multi-turn conversational chat running on the native engine.
- [ ] `nebula task` completes multi-turn coding tasks with real tools in Job Object sandboxes.
- [ ] 8 parallel candidate probe rollouts complete in strictly $< 40\text{ seconds}$ on RTX 4070.
- [ ] Zero regressions across existing test suites (`cargo nextest run --workspace --locked`).
