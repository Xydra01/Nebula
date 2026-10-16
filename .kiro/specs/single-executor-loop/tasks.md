# Implementation Plan: Single Executor Loop with Bounded Tool Calls (Issue #32)

## Overview

This plan delivers Nebula's **first agent loop** (GitHub issue #32): an autonomous, bounded executor that drives the local model and tool host inside a per-task git worktree and Windows Job Object to accomplish a stated goal. This satisfies the primary **Phase 1 exit criterion**: Nebula fixing a failing test in a small repo, end to end, on a branch in its own worktree.

## Tasks

- [x] 1. Wire protocol & identifiers (`nebula-proto`)
  - [x] 1.1 Add `TaskId` and `StepId` identifiers
    - In `crates/nebula-proto/src/ids.rs`, add `TaskId` and `StepId` via the `ulid_id!` macro
    - Re-export them in `crates/nebula-proto/src/lib.rs`
    - _Requirements: 1.1, 5.2_
  - [x] 1.2 Add wire params and events
    - In `crates/nebula-proto/src/methods.rs`, define `TaskCreateParams`, `TaskStarted`, and `TaskCancelParams`
    - In `crates/nebula-proto/src/events.rs`, define `TaskStatus`, `TaskStepEvent`, and `TaskDoneEvent`
    - Add `Method::TaskCreate` and `Method::TaskCancel` to the `Method` enum
    - Add `Event::TaskStep` and `Event::TaskDone` to the `Event` enum
    - _Requirements: 1.1, 4.1, 6.1, 6.3_
  - [x] 1.3 Bump `PROTO_VERSION` and update snapshots
    - Bump `PROTO_VERSION` from 2 to 3 in `crates/nebula-proto/src/envelope.rs`
    - Update `nebula-proto` snapshot tests via `INSTA_UPDATE=always cargo test -p nebula-proto`
    - _Requirements: 8.1_

- [x] 2. Configuration (`nebula-config` & `config/default.toml`)
  - [x] 2.1 Add `ExecutorConfig` struct and validation
    - In `crates/nebula-config/src/lib.rs`, add `ExecutorConfig` with `max_tool_calls` (default 15), `max_tokens` (default 40960), `wall_time_secs` (default 600), and `default_profile` (`Option<String>`)
    - Add `pub executor: ExecutorConfig` to `NebulaConfig`
    - Validate bounds in `NebulaConfig::validate()` (non-zero limits, safe ranges)
    - _Requirements: 3.1, 3.2, 3.5_
  - [x] 2.2 Update `config/default.toml`
    - Add the `[executor]` section with documented defaults
    - Update unit tests in `nebula-config`
    - _Requirements: 3.5, 8.4_

- [x] 3. Daemon task lifecycle & method dispatch
  - [x] 3.1 Task tracking and state in `Shared`
    - In `crates/nebula-daemon/src/lib.rs`, add running task tracking to `Shared` (storing cancellation token, task job handle, and join handle)
    - Ensure daemon shutdown triggers cancellation for all running tasks
    - _Requirements: 1.5, 4.4_
  - [x] 3.2 Wire IPC dispatch in `server.rs`
    - Handle `Method::TaskCreate` in `handle()`: validate inputs, spawn background execution, reply with `TaskStarted`
    - Handle `Method::TaskCancel` in `handle()`: cancel task token, terminate `TaskJob`, reply success or `NOT_FOUND`
    - _Requirements: 1.1, 1.4, 4.1, 4.5_

- [x] 4. Core executor implementation (`crates/nebula-daemon/src/executor.rs`)
  - [x] 4.1 Worktree and Job Object isolation setup
    - Create `crates/nebula-daemon/src/executor.rs`
    - Initialize task span with `trace_id` and `task_id`
    - Create per-task worktree via `shared.worktrees.create(...)`
    - Create per-task Job Object via `TaskJob::new(...)`
    - Seed conversation with system prompt (role, worktree cwd, tool instructions) and user prompt (goal)
    - _Requirements: 1.1, 1.2, 1.3, 5.1_
  - [x] 4.2 Inner model-tool loop with bounds enforcement
    - Wrap execution with wall-clock timeout (`tokio::time::timeout`)
    - Call model via `shared.chat.backend()?.chat(req)` advertising tools from `shared.tool_host.list_tools()`
    - Check token usage against budget; if exceeded, exit with `LimitReached`
    - If model requests tool calls:
      - Check tool-call count against `max_tool_calls`; if exceeded, exit with `LimitReached` without dispatching
      - Dispatch calls under `CURRENT_WORKTREE.scope` and `CURRENT_TASK_JOB.scope`
      - Append tool results back to message history (even if `is_error` or timeout)
      - Emit `Event::TaskStep`
    - If model outputs final answer with no tool calls: mark task as `Completed` with summary
    - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5, 3.1, 3.2, 3.3, 5.2, 5.3_
  - [x] 4.3 Task finalization and cleanup
    - Terminate Job Object processes (`task_job` dropped / kill-on-close)
    - Finalize worktree via `shared.worktrees.finalize(&task_id)` to preserve task branch for #36
    - Emit terminal `Event::TaskDone` with final status and summary
    - Remove task from daemon tracker
    - _Requirements: 3.4, 4.2, 4.3, 6.3, 7.2_

- [x] 5. CLI commands (`crates/nebula-cli`)
  - [x] 5.1 Implement `nebula task` CLI subcommands
    - Add `nebula task run <goal> [--repo <path>] [--profile <profile>]` command to CLI
    - Add `nebula task cancel <task-id>` command
    - Stream `task.step` events and print terminal summary on `task.done`
    - Update CLI snapshot tests
    - _Requirements: 1.1, 4.1, 6.1_

- [x] 6. Verification and integration tests
  - [x] 6.1 Executor unit & limit tests
    - Add integration tests verifying:
      - Reaching `max_tool_calls` cleanly stops with `LimitReached`
      - Reaching token budget cleanly stops with `LimitReached`
      - Wall-clock timeout cleanly stops with `LimitReached`
      - `task.cancel` promptly halts model calls and terminates Job Object processes
    - _Requirements: 3.1, 3.2, 3.3, 4.2_
  - [x] 6.2 Phase 1 exit criterion test (failing test fix end-to-end)
    - Create end-to-end integration test with a temporary git repository containing a failing test
    - Drive the loop with a scripted model backend simulating diagnosis, file edit, and test re-run
    - Verify tests pass on task branch and worktree finalizes cleanly
    - Add `#[ignore]` real-model test for live validation
    - _Requirements: 7.1, 7.2, 7.3_
  - [x] 6.3 Workspace gate verification
    - Run `cargo fmt --all --check`
    - Run `cargo clippy --workspace --all-targets --locked -- -D warnings`
    - Run `cargo nextest run --workspace --locked`
    - Run Python bench checks: `uv run ruff check .` and `uv run pytest -q`
    - _Requirements: 8.4_

