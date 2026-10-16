# Tasks: Circuit Breakers, Global Stop, and Disk Guard

- [x] 1. Configuration & Wire Protocol
  - [x] 1.1 Add `[circuit_breakers]` to `config/default.toml` (`max_repeated_tool_failures`, `max_file_edits`, `runaway_step_tokens`).
  - [x] 1.2 Add `CircuitBreakersConfig` struct with `deny_unknown_fields` to `crates/nebula-config/src/lib.rs`.
  - [x] 1.3 Add `Method::TaskStopAll` and associated params/responses in `crates/nebula-proto/src/methods.rs`.
  - [x] 1.4 Update `nebula-proto` snapshots if required.

- [x] 2. Job Object Immediate Termination
  - [x] 2.1 Implement `Job::terminate(&self, exit_code: u32)` in `crates/nebula-sandbox/src/job.rs` using `TerminateJobObject`.
  - [x] 2.2 Expose `TaskJob::terminate(&self)` in `crates/nebula-sandbox/src/task_job.rs`.
  - [x] 2.3 Unit test for job termination killing spawned child process trees immediately.

- [x] 3. Circuit Breaker Engine
  - [x] 3.1 Implement `CircuitBreakerTracker` in `crates/nebula-daemon/src/circuit_breaker.rs`.
  - [x] 3.2 Implement repeated identical tool failure detection (3 consecutive identical failures).
  - [x] 3.3 Implement repeated file edits detection (5 edits to the same file).
  - [x] 3.4 Implement runaway token detection per step.
  - [x] 3.5 Unit tests for each breaker tripping.

- [x] 4. Disk Guard Engine & Seam
  - [x] 4.1 Define `DiskSpaceReader` seam in `crates/nebula-daemon/src/executor.rs` or `disk_guard.rs`.
  - [x] 4.2 Check free space against volume budget (30 GB) before step execution.
  - [x] 4.3 Halt task with `TaskStatus::LimitReached` when space is below budget and log telemetry event.
  - [x] 4.4 Verify writes to `C:` are refused with explicit integration test.

- [x] 5. Global Stop Daemon & CLI Implementation
  - [x] 5.1 Store `Arc<TaskJob>` in `RunningTaskState` inside `crates/nebula-daemon/src/lib.rs`.
  - [x] 5.2 Implement `Method::TaskStopAll` in `crates/nebula-daemon/src/server.rs` cancelling all tokens and calling `job.terminate()`.
  - [x] 5.3 Add `nebula stop` subcommand to `crates/nebula-cli`.
  - [x] 5.4 Test verifying global stop terminates all processes in active task Job Objects within 2 seconds.

- [x] 6. Quality Gates & Verification
  - [x] 6.1 Run `cargo fmt --all`.
  - [x] 6.2 Run `cargo clippy --workspace --all-targets --locked -- -D warnings`.
  - [x] 6.3 Run `cargo nextest run --workspace --locked`.
  - [x] 6.4 Run bench tests: `cd bench; uv run ruff check .; uv run ruff format --check .; uv run pytest -q`.
  - [x] 6.5 Update `docs/ARCHITECTURE.md`.
