# Requirements: Circuit Breakers, Global Stop, and Disk Guard

## 1. Overview
Safety stops that do not depend on the model behaving. Ensures runaway processes, repeated loops, excessive resource consumption, and low-disk conditions immediately halt tasks, while providing a fast global stop mechanism to terminate all task processes within 2 seconds.

## 2. Circuit Breakers
- **Requirement 2.1 (Repeated Identical Tool Failures):** If the exact same tool call (identical tool name and arguments) fails $N$ consecutive times (default: 3), the circuit breaker trips.
- **Requirement 2.2 (Runaway Token Use):** If token usage surges abnormally in a single step (e.g. exceeds `runaway_step_tokens`, default 16,384 tokens), the breaker trips.
- **Requirement 2.3 (Repeated File Edits):** If the same file is modified repeatedly (default: 5 times) within a single task without completing or passing tests, the breaker trips.
- **Requirement 2.4 (Breaker Effect):** When any circuit breaker trips, the task halts cleanly with `TaskStatus::LimitReached`, logs a `circuit_breaker.tripped` telemetry event, and clearly documents the reason in the task summary.

## 3. Global Stop
- **Requirement 3.1 (CLI Command):** Top-level CLI command `nebula stop` cancels all active tasks immediately.
- **Requirement 3.2 (IPC Protocol):** Protocol method `task.stop_all` cancels every running task and triggers immediate termination of all processes in each task's Windows Job Object.
- **Requirement 3.3 (2-Second Process Termination):** All descendant processes in every task Job Object must terminate within 2 seconds of the stop signal.
- **Requirement 3.4 (Daemon & Model Preservation):** The daemon and loaded model profiles remain running and healthy after global stop to allow inspecting diagnostics.

## 4. Disk Guard
- **Requirement 4.1 (Low Space Halt):** Before each step or task execution, the executor inspects the free space of the drive hosting the worktree (or `F:`). If free space is below the budget (30 GB / `warn_gb`), the task pauses/halts with `TaskStatus::LimitReached` and logs a disk guard event.
- **Requirement 4.2 (Injectable Seam):** Disk space queries must support an injectable seam so tests can simulate low-space readings deterministically without filling the drive.
- **Requirement 4.3 (Retired Drive Refusal):** Writes to `C:` must be strictly refused by all tools and worktree operations.
