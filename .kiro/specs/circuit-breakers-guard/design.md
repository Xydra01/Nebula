# Design: Circuit Breakers, Global Stop, and Disk Guard

## 1. Architecture Overview

### Circuit Breaker Subsystem
- Located in `crates/nebula-daemon/src/circuit_breaker.rs`.
- `CircuitBreakerTracker` maintains per-task state:
  - `consecutive_tool_failures`: `Option<(String, u64, u32)>` (tool name, args hash, failure count).
  - `file_edit_counts`: `HashMap<PathBuf, u32>`.
  - Configured limits via `[circuit_breakers]` in `NebulaConfig`.
- Evaluated within `execute_loop`:
  - `check_step_tokens(step_tokens)` called after stream collection.
  - `record_tool_call(name, args, is_error)` called after each tool execution.
  - `record_file_edit(path)` called when a file write/edit tool is invoked.
- When tripped, returns `BreakerTrip` enum, sets `TaskStatus::LimitReached`, logs `circuit_breaker.tripped`, and terminates the loop with an explanatory summary.

### Global Stop Subsystem
- CLI: `nebula stop` subcommand in `crates/nebula-cli`.
- Protocol: `Method::TaskStopAll` (`task.stop_all`) in `crates/nebula-proto`.
- Daemon State:
  - In `crates/nebula-daemon/src/lib.rs`, `RunningTaskState` holds `cancel: CancellationToken` and `job: Option<Arc<TaskJob>>`.
  - In `crates/nebula-sandbox/src/job.rs`, implement `Job::terminate(&self, exit_code: u32)` calling Win32 `TerminateJobObject`.
  - In `crates/nebula-sandbox/src/task_job.rs`, implement `TaskJob::terminate(&self)`.
  - In `server.rs`, `handle(Method::TaskStopAll)` iterates over all `RunningTaskState`, cancels tokens, and calls `terminate()` on every job.
  - Synchronous process termination takes under 50ms, comfortably satisfying the 2-second SLA.

### Disk Guard Subsystem
- Executor checks worktree drive free space against `shared.config.resources.volumes` (specifically `F:\` with `warn_gb = 30`).
- An injectable seam (`DiskSpaceReader`) is passed into `LoopContext` / `execute_loop`.
- In production, reads real OS volume disk space via `nebula_resources`.
- In tests, injects a mock reader returning `< 30 GB` to verify immediate halting.
- Refusal of writes to `C:` is verified via `nebula-tools::path` integration tests.
