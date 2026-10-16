# Requirements: Single Executor Loop with Bounded Tool Calls

## Introduction

This feature builds Nebula's **first agent loop** (GitHub issue #32, design Section 5): a single
executor — no planner, no verifier yet — that takes a task, works it with the model and the tool
host until the task is done or a limit is hit, and reports the result. It is the **Phase 1 exit
criterion**: Nebula fixes a failing test in a small repo, end to end, inside a worktree.

The loop ties together everything built in #27–#31 and #33:

- the **model client** (`nebula-model`: `ModelBackend::chat`, schema-constrained tool calling);
- the **tool host** (`nebula-tools`: `ToolHost::list_tools` / `ToolHost::call`, with the per-call
  timeout, output cap, and the single `tool.call` event);
- the **per-task worktree** (#29: `WorktreeManager` + the `CURRENT_WORKTREE` task-local);
- the **per-task Job Object** (#30: `TaskJob` + the `CURRENT_TASK_JOB` task-local);
- the **permission tiers** (#28: already consulted inside `shell.run`/`git.*`);
- **telemetry** (per-task trace id threaded through every step).

The loop is driven by two new IPC methods, `task.create` and `task.cancel`, and a set of `task.*`
events streamed to the client, mirroring the existing `chat.start`/`chat.cancel`/`chat.*` design.

### Scope

**In scope (issue #32):**
- A `task.create` IPC method that starts one executor task and a `task.cancel` method that stops it.
- The executor loop: model call → schema-constrained tool calls → tool results back to the model,
  repeated until the model declares the task done, or a limit is hit, or the task is cancelled.
- Per-task limits: maximum tool calls, maximum model tokens, and wall-clock time. Hitting any limit
  stops the task cleanly with a `limit_reached` status and a summary — never a runaway loop.
- `task.cancel` stops the in-flight model call **and** all tool processes (via the task's Job
  Object), then cleans up.
- Every step logged with the task's trace id (and task/step ids).
- The task runs inside its own worktree (#29) and Job Object (#30), established by scoping the two
  task-locals around the loop.

**Explicitly out of scope (later issues):**
- The planner, verifier, reflector, summarizer, and the failure ladder (design 5.2–5.3) — this is a
  single executor only.
- The ledger / crash recovery of in-progress tasks (design 5.7).
- The memory layers and context compaction (design 5.4, 5.5) beyond a minimal context.
- The TUI task view (#31 / a separate issue).
- Opening a PR at the end of a task (#36). Finalize retains the branch (#29 `finalize`) for #36.
- Any change to the model wire protocol, the tool-host contract, or the permission engine.

## Requirements

### Requirement 1: `task.create` starts a bounded executor task

**User Story:** As a client (CLI/TUI), I want to start a task with a goal so that the executor
works it autonomously inside an isolated worktree.

#### Acceptance Criteria
1. WHEN a client sends `task.create` with a goal and a target repository THEN the daemon SHALL
   create a task record with a fresh task id and trace id, create a per-task worktree and branch via
   the #29 `WorktreeManager`, and reply with `TaskStarted { task_id, trace_id }` **before** any
   `task.*` event for that task is sent.
2. WHEN the task starts THEN the executor SHALL run inside the task's worktree root (scoped into
   `CURRENT_WORKTREE`) and the task's Job Object (scoped into `CURRENT_TASK_JOB`), so every built-in
   file/shell/git tool is confined to that worktree and every spawned process joins that job.
3. IF the worktree cannot be created (not a git repo, branch exists, retired-drive path, create
   failure) THEN the daemon SHALL fail `task.create` with an appropriate error code and start no
   executor task and leave no partial task record.
4. WHEN `task.create` omits an optional field with a configured default (profile, limits) THEN the
   executor SHALL use the configured default.
5. The executor task SHALL be tracked so `task.cancel` and daemon shutdown can reach it, in the same
   way chats are tracked today.

### Requirement 2: The bounded inner loop (model ↔ tools)

**User Story:** As the task owner, I want the executor to alternate model calls and tool calls until
the work is done, so that the model's decisions are actually carried out.

#### Acceptance Criteria
1. WHEN the loop runs THEN each iteration SHALL (a) call the model with the running conversation and
   the available tool definitions, (b) if the model requests tool calls, dispatch each through
   `ToolHost::call` and append the results as `tool`-role messages, and (c) repeat; otherwise end.
2. The tool definitions offered to the model SHALL be derived from `ToolHost::list_tools()` so the
   executor advertises exactly the tools the host can dispatch, each with its real input schema.
3. WHEN the model finishes a turn with no tool calls (a final answer / stop) THEN the task SHALL end
   with status `completed` and a short summary of the model's final message.
4. Tool-call arguments SHALL be validated by the host before dispatch (existing behavior); a tool
   that returns `is_error` SHALL be fed back to the model as a tool result (not abort the task), so
   the model can react — this is how the model fixes a failing test from the test output.
5. A tool call that the host rejects as unknown, times out, or that errors at the transport level
   SHALL be surfaced to the model as a tool result describing the failure, and the loop SHALL
   continue within the limits (no crash, no silent drop).

### Requirement 3: Per-task limits end the task cleanly as `limit_reached`

**User Story:** As the machine owner, I want hard bounds on a task so a confused model cannot loop
forever or exhaust the box.

#### Acceptance Criteria
1. The executor SHALL enforce a **maximum tool-call count** per task (configurable, design default
   15). WHEN the next model turn would exceed it THEN the task SHALL end with status `limit_reached`
   and a summary naming the limit; it SHALL NOT make the over-limit call.
2. The executor SHALL enforce a **maximum model-token budget** per task (summed across turns). WHEN
   the budget is exhausted THEN the task SHALL end with status `limit_reached`.
3. The executor SHALL enforce a **wall-clock timeout** per task. WHEN it elapses THEN the task SHALL
   end with status `limit_reached`, the in-flight model call SHALL be cancelled, and tool processes
   SHALL be stopped.
4. Reaching any limit SHALL stop the task **cleanly**: no further model or tool calls, a `task.done`
   event with `limit_reached`, and worktree finalize/cleanup as for any other ending.
5. All three limits SHALL come from config with documented defaults, and `task.create` MAY override
   them per task (clamped to safe ranges).

### Requirement 4: `task.cancel` stops the model call and all tool processes

**User Story:** As the task owner, I want to cancel a running task and have everything it started
stop promptly.

#### Acceptance Criteria
1. WHEN a client sends `task.cancel` with a known task id THEN the daemon SHALL cancel that task and
   reply success; an unknown task id SHALL return `NOT_FOUND`.
2. Cancelling SHALL stop the in-flight model call (drop/`select` on the cancellation token, as
   `run_chat` does) and SHALL terminate every process the task spawned by closing the task's Job
   Object (#30 kill-on-close).
3. WHEN cancelled THEN the task SHALL end with status `cancelled` and emit a `task.done` event; it
   SHALL NOT also emit `completed` or `limit_reached`.
4. Daemon shutdown SHALL cancel every running task the same way (via the task tracker), so no
   orphaned model call or tool process survives shutdown.
5. Cancelling a task that has already ended SHALL be a no-op success (idempotent), not an error.

### Requirement 5: Every step is logged with the task's trace id

**User Story:** As an operator, I want to follow a task end to end in the logs by its trace id.

#### Acceptance Criteria
1. The executor SHALL open a task span carrying `trace_id` and `task_id`, so every event it and its
   model/tool calls emit inherits them (mirroring the existing `chat` span).
2. Each loop iteration SHALL emit a step event (e.g. `task.step`) with a step id, the step index,
   and what happened (model turn / which tools were called), tagged with the trace id.
3. The model call SHALL carry the task's trace id (via `ChatRequest.trace_id`) and each tool call
   SHALL carry it (via `ToolHost::call(..., Some(trace_id))`), so the existing `model.call` and
   `tool.call` events correlate to the task.
4. Task start, each step, and task end SHALL be observable both as streamed `task.*` events to the
   client and as structured log events (so `logs.subscribe --trace-id <id>` shows the whole task).
5. No secret SHALL be logged or echoed in a `task.*` event (the existing telemetry redactor covers
   log events; the executor SHALL NOT place secret-bearing content into event payloads).

### Requirement 6: `task.*` events stream task progress to the client

**User Story:** As a client, I want to watch a task's progress on the pipe, interleaved with the
`task.create` response, the way chat tokens stream today.

#### Acceptance Criteria
1. The daemon SHALL define `task.*` events (at least: a step/progress event and a terminal
   `task.done` event with the final status and a summary), added to the `Event` enum and its
   `name()`, mirroring `chat.token`/`chat.done`/`chat.error`.
2. Events for a task SHALL be streamed on the originating connection's outbox after the
   `TaskStarted` reply, so a client reading responses must skip `Notification`s (the documented
   `next_response()` pattern) — unchanged client contract.
3. The terminal `task.done` event SHALL carry the final status (`completed` | `limit_reached` |
   `cancelled` | `failed`) and a short human-readable summary.
4. A task that fails for an internal reason (model unavailable, worktree error mid-task) SHALL end
   with status `failed` and a `task.done` carrying an error message, not a panic or a hang.

### Requirement 7: Phase 1 exit — fix a failing test end to end

**User Story:** As the project owner, I want the headline Phase 1 capability demonstrated: Nebula
fixes a failing test in a small repo, inside a worktree, on its own.

#### Acceptance Criteria
1. GIVEN a small repo with one failing test and a task whose goal is to fix it, WHEN the task runs
   THEN the executor SHALL be able to: read the failing test and source (fs/`shell`/`git` tools),
   run the tests (`test.run` from #33, or `shell.run`), edit the source (`fs.write`), re-run the
   tests, and end `completed` once the tests pass — all within the worktree and the limits.
2. The change SHALL be made on the task's branch in the task's worktree; finalize SHALL retain that
   branch for #36 (no push/PR/merge — AGENTS.md hard rule 1).
3. This end-to-end capability SHALL be demonstrated by an integration test (gated on the toolchain
   and a model being available, skipping cleanly when not) and/or a documented manual runbook step.

### Requirement 8: No regressions to existing wire, tool, or permission contracts

**User Story:** As a maintainer, I want the executor added without destabilizing the shipped
foundations.

#### Acceptance Criteria
1. Adding `task.create`/`task.cancel`/`task.*` SHALL bump `PROTO_VERSION` and update the proto and
   CLI snapshot tests, since it is an incompatible wire addition.
2. The executor SHALL use the existing `ToolHost::call` and `ModelBackend::chat` contracts unchanged
   — no new tool-host method, no change to the model request/response types beyond what already
   exists.
3. The permission engine, worktree manager, and task-job seam SHALL be **used** as-is; any change to
   `nebula-sandbox` (protected set, AGENTS.md hard rule 9) SHALL be avoided, or if unavoidable kept
   minimal and flagged for close review.
4. The full workspace gate (`cargo fmt --all --check`, `clippy -D warnings`, `cargo nextest run
   --workspace`) and the devtools gate SHALL pass; no panics/`unwrap`/`expect` in library code
   outside tests (AGENTS.md).

## Open Questions (with recommendations)

1. **Where does the executor live — a new `nebula-orchestrator` crate or a `nebula-daemon` module?**
   The design's crate layout (Section 4) anticipates `nebula-orchestrator` for the full
   planner/executor/verifier. But #32 is deliberately a single executor with no planner, and the
   loop needs `Shared` (model manager, tool host, worktree manager, telemetry) which all live in
   `nebula-daemon`.
   **Recommendation:** build it as a module in `nebula-daemon` (`src/executor.rs`) for now, with a
   clean internal boundary (an `Executor` struct taking the handles it needs), and extract
   `nebula-orchestrator` later when the planner/verifier arrive. `nebula-daemon` is not protected;
   this keeps the PR small and avoids a premature crate. (Alternative: create the crate now —
   rejected: more churn, a dependency-cycle risk against `Shared`, and no planner to justify it yet.)

2. **What does `task.create` take as the task goal and repo?**
   **Recommendation:** `TaskCreateParams { goal: String, repo: PathBuf, start_point: Option<String>,
   profile: Option<String>, max_tool_calls: Option<u32>, max_tokens: Option<u32>,
   wall_time_secs: Option<u64> }`. `goal` is the user instruction; `repo` is the target repository
   the worktree is cut from; limits/profile are optional overrides. (Alternative: infer the repo
   from a config "projects" list — rejected: no such registry exists yet; explicit is simpler.)

3. **How is the task's conversation seeded (context assembly)?**
   Design 5.5 describes a budgeted template, but that is a later issue.
   **Recommendation:** a minimal seed — one system message (role instructions + the worktree is the
   cwd + available tools are advertised via the schema) and one user message (the goal). No memory,
   no retrieval. Keep it a single private function so the richer assembly can replace it later.
   (Alternative: implement part of 5.5 now — rejected: out of scope, risks scope creep.)

4. **Reasoning effort for the executor's model calls?**
   Design 5.5.1: executor mechanical steps use `reasoning: none` with the server at `--reasoning
   auto`; `max_tokens` ~4K headroom.
   **Recommendation:** default the executor's `ChatRequest.reasoning` to `None` and `max_tokens` to
   the existing `DEFAULT_MAX_TOKENS` (4096), overridable by config. (Alternative: `medium` —
   rejected: that is for planner/verifier roles, costs the token headroom the design warns about.)

5. **Does the executor use schema-constrained decoding for tool calls, or native tool calling?**
   The backend already passes `tools` to llama-server (OpenAI function-calling) **and** supports a
   `response_schema`. The model emits `StreamItem::ToolCall` natively.
   **Recommendation:** use the backend's existing native tool-calling path (`ChatRequest.tools` from
   `list_tools()`, consume `ChatOutput.tool_calls`). The host already validates arguments against
   each tool's schema before dispatch, which is the "malformed calls become impossible" guarantee at
   the dispatch boundary. Do **not** also set a `response_schema` (the two mechanisms conflict).
   (Alternative: force a custom response schema — rejected: duplicates the tool mechanism and fights
   the backend's function-calling.)

6. **Where do the executor limits live in config?**
   **Recommendation:** a new `[executor]` table in `config/default.toml` + a typed `ExecutorConfig`
   in `nebula-config` (`max_tool_calls`, `max_tokens`, `wall_time_secs`, `default_profile?`), with
   `deny_unknown_fields` and range validation. Keep the #30 `[tasks]` Job-Object caps separate (they
   feed `JobLimits`); `[executor]` is about the loop's model/tool bounds. (Alternative: fold into
   `[tasks]` — rejected: conflates process-resource caps with loop logic; separate tables read
   clearer.)

7. **What repository does the Phase-1-exit test run against, given AGENTS.md "never touch C:", the
   worktree-dir on `F:`, and tests never touching real paths?**
   **Recommendation:** the integration test builds a throwaway git repo in a `tempfile` dir (with a
   failing test), points the `WorktreeManager` at a temp `worktrees_dir`, and uses a **fake or
   recorded model backend** so the loop is deterministic and needs no GPU — asserting the loop
   mechanics (bounded calls, tool dispatch, `limit_reached`, cancel). A separate `#[ignore]`d test
   (or a runbook step) exercises the real model for the true end-to-end demo. (Alternative: require a
   live model in CI — rejected: no GPU in CI, non-deterministic; AGENTS.md marks GPU tests
   `#[ignore]`.)

8. **Does `task.cancel` need a shared `ApprovalStore`, or any approval wiring?**
   The executor runs tools at their existing tiers; `shell.run` consults the classifier itself.
   **Recommendation:** no new approval wiring in #32. Sandbox/Read-tier tools (fs, test.run, the
   common edit/test tools) need no approval, which is enough for the Phase-1-exit flow. Above-tier
   commands will simply be refused by the classifier (as today) until the approval UX lands with the
   TUI. (Alternative: wire an auto-approver — rejected: that is a deliberate human-in-the-loop
   decision, out of scope and a safety regression.)
