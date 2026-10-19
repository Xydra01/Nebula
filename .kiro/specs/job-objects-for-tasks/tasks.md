# Implementation Plan: Job Objects for Tasks (one Job Object per task)

## Overview

This plan builds the per-task Windows **Job Object** mechanism for Nebula (GitHub issue #30, Phase 1 exit criterion 3), consolidating the two duplicate per-process `job` modules (in `nebula-model/src/launcher.rs` and `nebula-tools/src/launcher.rs`) into one audited `unsafe` Win32 surface in the `nebula-sandbox` crate, and adding the per-task `TaskJob` plus the spawn seam that groups every process a task starts into one kill-on-close job. It is a protected-set, safety-critical feature (AGENTS.md hard rule 9), so tasks are strictly incremental and test-driven: the consolidated `job` module lands first (the single `unsafe` surface), then the completion port, then the suspended-spawn plumbing, then `TaskJob` with limits and the reason pump, then the `CURRENT_TASK_JOB` task-local and the `spawn_in_task_job` seam, then the `nebula-model` migration (checkpoint keeps model-server supervision green), then the `nebula-tools` migration (checkpoint keeps issue #27 green), then the `[tasks]` config key, then the property and Windows-gated integration tests, and finally a checkpoint and a (never-merged) PR.

This feature assumes the `nebula-sandbox` crate already exists (created by issue #28, also home to issue #29's worktree module). Issue #30 shares only the crate with #28/#29; it is otherwise independent of #28's classifier and #29's worktree logic and can be built in parallel once the crate exists.

The consolidation is the keystone: the four job-object FFI calls (`CreateJobObjectW`, `SetInformationJobObject`, `AssignProcessToJobObject`, and the job-handle `CloseHandle`) must appear in exactly one module after this feature, and `nebula-model` and `nebula-tools` both build on it rather than carrying their own copies (Requirement 9).

The design defines 10 correctness properties (P1–P10). Each is implemented as a single property test — a `proptest` with a minimum of 100 iterations where a generated input space applies, or a Windows-gated integration test that generates process-tree shapes against real spawned processes where the quantifier ranges over OS behavior — placed next to the code it verifies and tagged `// Feature: job-objects-for-tasks, Property N: ...` with a `Validates: Requirements X.Y` annotation. The heaviest acceptance test (a real `taskkill /F` of a daemon-like host, Phase 1 exit criterion 3) is an `#[ignore]`d real-machine test per AGENTS.md.

Language: Rust (used throughout the design and the existing crates). Conventions enforced per task: `thiserror` in libraries, `anyhow` only in binaries, no `unwrap`/`expect`/`panic` in library code (clippy deny; test files carry the allow header), `unsafe` denied everywhere EXCEPT the one isolated `nebula-sandbox::job` module where every `unsafe` block carries a `// SAFETY:` comment, doc comments on every public item and a module doc on each `lib.rs`, serde structs with `#[serde(deny_unknown_fields)]`, any new config key in BOTH `config/default.toml` and `nebula-config`, and all tests redirect paths to tempdirs and never touch real paths or the retired `C:` drive. Target: Windows MSVC.

## Tasks

- [ ] 1. Consolidated `nebula-sandbox::job` module — the one audited unsafe Win32 surface
  - [ ] 1.1 Add the `windows` crate features and declare the `job` module
    - In `crates/nebula-sandbox/Cargo.toml`, under `[target.'cfg(windows)'.dependencies]`, add the `windows` crate features `Win32_Foundation`, `Win32_Security`, `Win32_System_JobObjects`, `Win32_System_Threading` (as used by `nebula-tools` today) plus `Win32_System_IO` (completion ports) and `Win32_System_Diagnostics_ToolHelp` (main-thread lookup)
    - In `crates/nebula-sandbox/src/lib.rs`, declare `#[cfg(windows)] #[allow(unsafe_code)] pub mod job;` with a crate-level note that this is the single audited `unsafe` job surface; everywhere else `unsafe` stays denied
    - Create `crates/nebula-sandbox/src/job.rs` with a module doc stating it owns `CreateJobObjectW`, `SetInformationJobObject`, `AssignProcessToJobObject`, the job-handle `CloseHandle`, `CreateIoCompletionPort`/`GetQueuedCompletionStatus`, and the `CREATE_SUSPENDED` + `ResumeThread` plumbing, and that no other module declares these FFI calls (Requirement 9.1, 9.5)
    - _Requirements: 9.1, 9.5_

  - [ ] 1.2 Implement `Job` (create, limits, assign, Drop) and `MemoryScope`
    - In `job.rs`, define `pub struct Job(HANDLE)` with `// SAFETY:` `unsafe impl Send`/`unsafe impl Sync` (a job handle is a kernel handle usable from any thread), and `pub enum MemoryScope { PerProcess, WholeJob }`
    - Implement `pub fn new_kill_on_close() -> io::Result<Self>`: `CreateJobObjectW(None, null)` then `SetInformationJobObject` with `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` carrying `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` only (no breakaway flag — Requirement 3.4); each `unsafe` block preceded by a `// SAFETY:` comment stating the invariant
    - Implement `pub fn new_with_limits(memory: Option<(u64, MemoryScope)>, cpu_rate_hundredths_percent: Option<u32>) -> io::Result<Self>`: kill-on-close plus `JOB_OBJECT_LIMIT_PROCESS_MEMORY` and/or `JOB_OBJECT_LIMIT_JOB_MEMORY` when a memory cap is given, and a second `SetInformationJobObject` with `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION` (`ENABLE | HARD_CAP`, `CpuRate` in hundredths of a percent) when a CPU cap is given — all applied before this returns so no member can be assigned before the limits are in force (Requirement 6.4, 7.1, 8.1)
    - Implement `pub fn assign(&self, process: RawHandle) -> io::Result<()>` via `AssignProcessToJobObject` (does not duplicate an inheritable job handle into the process — Requirement 6.5), and `impl Drop for Job` closing the handle exactly once via `CloseHandle`
    - Never set `JOB_OBJECT_LIMIT_BREAKAWAY_OK` or `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK` so membership is always inherited and breakaway is denied (Requirement 3.1, 3.4); doc comments on every public item; no `unwrap`/`expect`/`panic`
    - _Requirements: 1.1, 3.1, 3.4, 6.4, 6.5, 7.1, 8.1, 9.1, 9.2_
    - _Properties: P10_

  - [ ]* 1.3 Unit tests — job creation and limit flags
    - Assert `new_kill_on_close` sets the kill-on-close flag and NO breakaway flag; `new_with_limits` sets the per-process vs whole-job memory flag only when configured and the CPU-rate info only when configured; a job with no limits carries no memory/CPU control
    - Windows-gated (`#[cfg(windows)]`); no real user path or `C:` touched
    - _Requirements: 1.1, 3.4, 7.1, 8.1, 8.2_

- [ ] 2. Completion port and termination-reason mapping
  - [ ] 2.1 Implement `CompletionPort` and `TerminationReason`
    - In `job.rs`, implement `pub struct CompletionPort(HANDLE)` with `pub fn associate(job: &Job) -> io::Result<Self>` (`CreateIoCompletionPort` then `SetInformationJobObject` with `JOBOBJECT_ASSOCIATE_COMPLETION_PORT`) and `pub fn next_message(&self) -> io::Result<Option<(u32, u32)>>` (`GetQueuedCompletionStatus` on a dedicated blocking task, returning the raw message code and member PID, and `Ok(None)` when the port is closed); `// SAFETY:` on every `unsafe` block; `Drop` closes the port handle
    - In the sandbox lib (e.g. `src/task_job.rs`, declared `pub mod task_job;`), define `pub enum TerminationReason { MemoryLimit { limit_bytes: u64, scope: job::MemoryScope }, ProcessCrashed }` with doc comments
    - Implement a pure `fn reason_for(msg_code: u32, limit_bytes: u64, scope: MemoryScope) -> Option<TerminationReason>`: `JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT`/`JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` → `MemoryLimit { .. }`, `JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS` → `ProcessCrashed`, `JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO` and others → `None`
    - _Requirements: 7.5_
    - _Properties: P8_

  - [ ]* 2.2 Property test — a job message maps to the correct termination reason
    - **Property 8: A memory-limit termination surfaces the correct reason**
    - **Validates: Requirements 7.3, 7.4, 7.5**
    - `proptest` over generated `(message code, limit_bytes, scope)` tuples asserting `reason_for` yields `TerminationReason::MemoryLimit` carrying the applied limit and scope for the two memory-limit messages, `ProcessCrashed` for abnormal exit, and `None` otherwise (the mapping half of Property 8; the real-OS half is the Windows-gated test in task 9.5); min 100 iterations
    - Tag: `// Feature: job-objects-for-tasks, Property 8: a memory-limit termination surfaces the correct reason`
    - Place in `crates/nebula-sandbox/src/task_job.rs` tests
    - _Properties: P8_

- [ ] 3. Suspended-spawn plumbing (assign before the child can escape)
  - [ ] 3.1 Implement `spawn_suspended` and `ResumeHandle`
    - In `job.rs`, implement `pub fn spawn_suspended(cmd: &mut tokio::process::Command) -> io::Result<(tokio::process::Child, ResumeHandle)>`: add `CREATE_SUSPENDED | CREATE_NO_WINDOW` via `cmd.creation_flags(..)`, `cmd.spawn()`, then find the child's main thread with `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)` + `Thread32First`/`Thread32Next` filtered to `th32OwnerProcessID == child.id()` (a freshly `CREATE_SUSPENDED` process has exactly one thread, so no ambiguity), then `OpenThread(THREAD_SUSPEND_RESUME, false, tid)` and wrap the thread handle in `ResumeHandle`
    - Implement `pub struct ResumeHandle(HANDLE)` with `pub fn resume(self) -> io::Result<()>` calling `ResumeThread` then `CloseHandle`, and a `Drop` that closes the thread handle if `resume` was never called (so a dropped-without-resume child's thread handle is not leaked; the child itself is killed via the job)
    - Every `unsafe` block preceded by a `// SAFETY:` comment; this is the trickiest unsafe part of the feature — keep the whole spawn on the `tokio::process` path (normal `Child` with piped stdio and `kill_on_drop`); doc comments on every public item
    - _Requirements: 4.1, 4.2, 4.3, 9.1, 9.2_
    - _Properties: P5_

  - [ ]* 3.2 Unit test — a suspended child does not run until resumed
    - Windows-gated: spawn a child via `spawn_suspended` that would write a sentinel file on start; assert the sentinel does not appear while suspended, appears after `resume()`, and that dropping the `ResumeHandle` without resuming leaks no thread handle (and the child is killed when its job closes)
    - All paths redirected to tempdirs; never touch real paths or `C:`
    - _Requirements: 4.1, 4.2, 4.3_

- [ ] 4. `TaskJob` — one job per task, with limits and the reason pump
  - [ ] 4.1 Define `TaskId`, `JobLimits`, `MemoryLimit`, `CpuRateLimit`, and `JobError`
    - In `crates/nebula-sandbox/src/task_job.rs`, define `JobLimits { memory: Option<MemoryLimit>, cpu: Option<CpuRateLimit> }` (both default off), `MemoryLimit { bytes: u64, scope: job::MemoryScope }`, and `CpuRateLimit { hundredths_percent: u32 }`, each with doc comments; reference or define the shared `TaskId` type used by the executor
    - Define `pub enum JobError` (`thiserror`): `Create(String)`, `LimitOutOfRange { bytes, physical }`, `NoTaskInScope`, `Spawn(String)`, `Assign { task_id, detail }`, `ExitedBeforeAssign`, `InheritableHandlePrevented`, `CloseFailed { task_id, detail }`, `Survivors { task_id, survivors }` — the single Tool-level error the Task_Job API returns (Requirement 10.5)
    - _Requirements: 7.3, 7.4, 8.1, 10.5_

  - [ ] 4.2 Implement `TaskJob::new`, `task_id`, Drop, and the limit range check
    - Implement `pub fn new(task_id: TaskId, limits: JobLimits) -> Result<Self, JobError>`: reject a memory limit `< 1` byte or `>` installed physical memory with `JobError::LimitOutOfRange` BEFORE creating any job (Requirement 7.2); otherwise build the kill-on-close job via `job::Job::new_with_limits` with the limits applied before any member can be assigned (Requirement 1.1, 6.4, 7.1, 8.1), mapping an OS failure to `JobError::Create` with no job left behind (Requirement 1.4)
    - Store the owning `Job`, the `task_id`, and the `JobLimits`; implement `pub fn task_id(&self) -> &TaskId` and `impl Drop` so dropping the last handle closes it and kill-on-close terminates the tree (Requirement 1.5, 5.1, 5.3, 6.1); the handle is non-inheritable and never duplicated into a child (Requirement 6.1, 6.5)
    - Read installed physical memory through a small helper (not hard-coded); no `unwrap`/`expect`/`panic`
    - _Requirements: 1.1, 1.4, 1.5, 5.1, 5.3, 6.1, 6.4, 6.5, 7.1, 7.2, 7.6, 8.1, 8.2_
    - _Properties: P9_

  - [ ] 4.3 Wire the completion-port pump and `next_termination_reason`
    - When a memory limit is configured, in `TaskJob::new` associate a `CompletionPort` with the job and spawn a dedicated blocking pump task that calls `CompletionPort::next_message` in a loop, maps each code via `reason_for`, and forwards any `TerminationReason` onto a channel (Requirement 7.5)
    - Implement `pub async fn next_termination_reason(&self) -> Option<TerminationReason>` returning the next surfaced reason, or `None` when no limits/pump are configured; the pump logs only PIDs, message codes, limits, and the `Task_Id` (no secrets); on `Drop`, close the port so the pump exits
    - _Requirements: 7.5_
    - _Properties: P8_

  - [ ]* 4.4 Property test — memory-limit configuration is accepted exactly within range
    - **Property 9: Memory-limit configuration is accepted exactly within range**
    - **Validates: Requirements 7.2**
    - `proptest` over generated memory-limit values around the boundaries (`0`, `1`, `physical`, `physical + 1`, and random values); assert `TaskJob::new` creates the job iff the value is `1..=physical` and otherwise returns `JobError::LimitOutOfRange` leaving no job created; min 100 iterations
    - Tag: `// Feature: job-objects-for-tasks, Property 9: memory-limit configuration is accepted exactly within range`
    - _Properties: P9_

- [ ] 5. The per-task context and spawn seam
  - [ ] 5.1 Define the `CURRENT_TASK_JOB` task-local and `JobChild`
    - In `crates/nebula-sandbox/src/task_job.rs`, declare `tokio::task_local! { pub static CURRENT_TASK_JOB: Option<std::sync::Arc<TaskJob>>; }` mirroring #29's `CURRENT_WORKTREE` (the executor sets BOTH in one scope); document that `None`/unset means no task is in scope
    - Define `pub struct JobChild { child: tokio::process::Child, _job: std::sync::Arc<TaskJob> }` with `pub fn pid(&self) -> Option<u32>` and `pub async fn wait_with_timeout(&mut self, timeout: Duration) -> std::io::Result<Option<std::process::Output>>`, mirroring issue #27's `JobChild`; holding the guard does not keep the job alive beyond the owning `Arc<TaskJob>`
    - _Requirements: 10.1, 10.2_

  - [ ] 5.2 Implement `spawn_in_task_job`
    - Implement `pub fn spawn_in_task_job(cmd: &mut tokio::process::Command) -> Result<JobChild, JobError>`: read `CURRENT_TASK_JOB`; `None`/unset → return `JobError::NoTaskInScope` spawning NOTHING (Requirement 10.4, fail-closed); `Some(job)` → `job::spawn_suspended(cmd)` (adds `CREATE_SUSPENDED | CREATE_NO_WINDOW` and `kill_on_drop(true)`), then `job.assign(child.raw_handle())` while still suspended (Requirement 2.1, 4.1), then `ResumeHandle::resume()` on the `Ok` path only (Requirement 4.3, 4.4)
    - On `spawn_suspended` failure → `JobError::Spawn`; on assign failure → terminate the still-suspended child without ever resuming it (no descendants exist) and return `JobError::Assign` (Requirement 2.6, 4.5, 4.6); if the suspended child exited before assignment can be confirmed → terminate and return `JobError::ExitedBeforeAssign`, not reporting a successful spawn (Requirement 2.7); clone the `Arc<TaskJob>` only to read the job for assignment and drop it into the returned `JobChild`, never holding a job-handle duplicate open past the spawn (Requirement 6.5)
    - _Requirements: 2.1, 2.3, 2.4, 2.5, 2.6, 2.7, 4.1, 4.2, 4.3, 4.4, 4.5, 4.6, 10.1, 10.2, 10.3, 10.4_
    - _Properties: P2, P5, P6_

  - [ ]* 5.3 Unit test — no task in scope spawns nothing
    - With `CURRENT_TASK_JOB` unset, `spawn_in_task_job` returns `JobError::NoTaskInScope` and starts zero processes (use a spy/sentinel to confirm no child was spawned)
    - _Requirements: 10.4_

  - [ ]* 5.4 Unit test — assign-failure terminates the suspended child before resume
    - Windows-gated: inject an assign failure after a suspended spawn; assert the child is terminated before resume, never ran user code, and left no descendant; the error is `JobError::Assign`
    - _Requirements: 2.6, 4.5, 4.6_

- [ ] 6. Migrate `nebula-model` to the shared `Job`
  - [ ] 6.1 Replace the private `job` module in `nebula-model/src/launcher.rs`
    - Add a `nebula-model → nebula-sandbox` dependency edge in `crates/nebula-model/Cargo.toml`
    - In `crates/nebula-model/src/launcher.rs`, delete the private `#[cfg(windows)] mod job` and build each llama-server's per-process, daemon-lifetime job from `nebula_sandbox::job::Job::new_kill_on_close()` + `assign(child.raw_handle())`; keep the `_job: Job` field on `Process` so Drop order still closes the job after the child
    - Behavior unchanged (each model server in its own daemon-lifetime job); the model launcher does NOT use `TaskJob`, the completion-port pump, or the suspended-spawn seam — a model server is not a task and is never a member of any Task_Job (Requirement 9.4, 11.4)
    - _Requirements: 9.4, 11.4_
    - _Properties: P7, P10_

- [ ] 7. Checkpoint — model-server supervision stays green on the shared Job
  - Confirm `cargo build -p nebula-sandbox -p nebula-model` and `cargo nextest run -p nebula-model --locked` pass; model-server launch/stop behavior is unchanged and no model-server process becomes a Task_Job member
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 8. Migrate `nebula-tools` to the shared surface and remove its `job` module
  - [ ] 8.1 Point `StdioLauncher` at the shared `Job` (tool servers stay daemon-lifetime)
    - In `crates/nebula-tools/src/launcher.rs`, change `StdioLauncher` to build its daemon-lifetime per-process tool-server job from `nebula_sandbox::job::Job` — a tool server is launched once and shared across tasks, so it is NOT spawned through the per-task seam and is never placed in a Task_Job (settled decision M; refinement of Requirement 2.2)
    - _Requirements: 2.2, 9.4_

  - [ ] 8.2 Migrate `shell.run` and `git.*` to `spawn_in_task_job`
    - In `crates/nebula-tools/src/builtins/shell.rs` and `builtins/git.rs`, switch the child-spawn call sites from `launcher::child::spawn_in_job` to `nebula_sandbox::spawn_in_task_job`, which reads `CURRENT_TASK_JOB`; keep the call sites otherwise as they are (`current_dir`, env scrubbing, timeout handling via `wait_with_timeout`)
    - A `shell.run` / `git.*` call made with a task in scope spawns its child (and any descendants, by inherited membership) into that task's Task_Job (Requirement 2.3); with no task in scope the seam fails closed with `NoTaskInScope`
    - _Requirements: 2.3, 2.4, 2.5, 3.1_

  - [ ] 8.3 Remove the duplicate `job` module and per-process spawn from `nebula-tools/src/launcher.rs`
    - Delete `pub(crate) mod job` and the per-process part of `mod child` (`spawn_in_job`/the per-process `JobChild`) now that both have shared replacements; leave zero per-process task-scope job modules in `nebula-tools/src/launcher.rs` (Requirement 9.4)
    - Confirm `nebula-tools` has no remaining `unsafe` Win32 job FFI; it obtains all job behavior through `nebula-sandbox` (Requirement 9.5)
    - _Requirements: 9.4, 9.5_
    - _Properties: P10_

- [ ] 9. Property and Windows-gated integration tests
  - [ ]* 9.1 Property test — assignment completeness
    - **Property 2: Assignment completeness — every task-spawned process is in exactly its task's job**
    - **Validates: Requirements 2.1, 2.3, 2.4, 2.5**
    - Windows-gated integration test generating sets of children spawned through the seam under a `TaskJob`; use `IsProcessInJob` to assert each spawned process is a member of exactly that task's job and of no other, and none is left unassigned (tool-server processes are excluded by design); the generated set size/shape exercises the "for all" quantifier
    - Tag: `// Feature: job-objects-for-tasks, Property 2: assignment completeness`
    - _Properties: P2_

  - [ ]* 9.2 Integration test — no survivors after a Task_Job closes (shell → grandchild)
    - **Property 1: No survivors after a Task_Job closes**
    - **Validates: Requirements 3.1, 3.2, 5.1, 5.3, 6.3**
    - Windows-gated: generate a process tree where a `shell.run`-style child launches a grandchild at least two levels deep; confirm via `IsProcessInJob` the grandchild was a member before the kill (never escaped), then close the `TaskJob` (drop the last `Arc`) and assert ZERO survivors within 2 seconds of the cancellation request (decision Q), across generated tree shapes/depths
    - Tag: `// Feature: job-objects-for-tasks, Property 1: no survivors after a Task_Job closes`
    - _Properties: P1_

  - [ ]* 9.3 Integration test — assign-before-escape
    - **Property 5: Assign-before-escape — a child cannot spawn an unassigned descendant**
    - **Validates: Requirements 3.4, 4.1, 4.2, 4.3, 4.5, 4.6, 10.2**
    - Windows-gated: spawn through the seam a child that would immediately fork a descendant; assert the child stays suspended until assigned, so every descendant it later spawns is already a member (`IsProcessInJob`); assert a child attempting `CREATE_BREAKAWAY_FROM_JOB` remains a member (breakaway denied, Requirement 3.4); and assert an injected assign failure terminates the child without resume leaving no descendant
    - Tag: `// Feature: job-objects-for-tasks, Property 5: assign-before-escape`
    - _Properties: P5_

  - [ ]* 9.4 Integration test — isolation across tasks
    - **Property 3: Isolation — closing one job never affects another task or the daemon**
    - **Validates: Requirements 11.1, 11.2**
    - Windows-gated: generate two distinct task trees plus a sibling non-member process; close one task's `TaskJob` and assert only that job's members die while the other task's members, the sibling process, and the test host itself all survive
    - Tag: `// Feature: job-objects-for-tasks, Property 3: isolation`
    - _Properties: P3_

  - [ ]* 9.5 Integration test — memory limit surfaces the correct reason
    - Windows-gated (completes the real-OS half of Property 8): a member runs a runaway allocator under a configured `MemoryLimit`; assert the OS terminates the offending member (per-process scope) or the whole job (whole-job scope) and that `next_termination_reason()` yields `TerminationReason::MemoryLimit` carrying the applied limit and scope within 5 seconds, distinct from an ordinary exit
    - Tag: `// Feature: job-objects-for-tasks, Property 8: a memory-limit termination surfaces the correct reason`
    - _Requirements: 7.3, 7.4, 7.5_
    - _Properties: P8_

  - [ ]* 9.6 Property test — handle-count bookkeeping, no inherited job handle
    - **Property 6: The job handle count is unchanged by spawning and no child inherits a job handle**
    - **Validates: Requirements 6.1, 6.4, 6.5, 6.6**
    - Windows-gated: generate a sequence of spawns into a `TaskJob`; assert the count of open handles to the job after each spawn equals the count before, no spawned child holds a handle to the job, and the host holds exactly one long-lived non-inheritable handle (so closing it triggers kill-on-close); min 100 generated spawn sequences
    - Tag: `// Feature: job-objects-for-tasks, Property 6: the job handle count is unchanged by spawning`
    - _Properties: P6_

  - [ ]* 9.7 Integration test — only task-started processes are affected
    - **Property 7: Only task-started processes are ever affected**
    - **Validates: Requirements 11.3, 11.4**
    - Windows-gated: alongside a task tree, run a `nebula-model`-style per-process-job model server and an owner-style process started outside any task; close the Task_Job and assert neither the model server nor the owner process is assigned to any Task_Job or terminated
    - Tag: `// Feature: job-objects-for-tasks, Property 7: only task-started processes are ever affected`
    - _Properties: P7_

  - [ ]* 9.8 Source-scan test — the audited unsafe surface is unique and fully commented
    - **Property 10: The audited unsafe surface is unique and fully commented**
    - **Validates: Requirements 9.1, 9.2, 9.4, 9.5**
    - A test (or `proptest` over the crate source set) scanning the workspace sources: assert the four job FFI symbols (`CreateJobObjectW`, `SetInformationJobObject`, `AssignProcessToJobObject`, job-handle `CloseHandle`) appear only in `nebula-sandbox::job`, that module's `unsafe {` count equals its `// SAFETY:` count, and no `job` module remains in `nebula-model`/`nebula-tools` launchers (Requirement 9.4)
    - Tag: `// Feature: job-objects-for-tasks, Property 10: the audited unsafe surface is unique and fully commented`
    - _Properties: P10_

  - [ ]* 9.9 Real-machine test — daemon death leaves zero survivors (`#[ignore]`d)
    - **Property 4: Daemon death leaves zero task-process survivors**
    - **Validates: Requirements 6.1, 6.2, 6.3**
    - `#[ignore]`d real-machine test (per AGENTS.md, run locally with `cargo nextest run --run-ignored all`): launch a daemon-like host that creates a `TaskJob` and a task process tree, `taskkill /F` the host, and assert zero surviving members within 10 seconds (Phase 1 exit criterion 3) — the host must hold exactly one long-lived non-inheritable handle so kill-on-close fires on crash
    - Tag: `// Feature: job-objects-for-tasks, Property 4: daemon death leaves zero task-process survivors`
    - _Properties: P4_

- [ ] 10. Configuration — the `[tasks]` section
  - [ ] 10.1 Add `[tasks]` keys to config/default.toml and nebula-config
    - Add a `[tasks]` section to `config/default.toml` with `memory_limit_mib = 0` (0 = no cap), `memory_limit_scope = "job"` (`"process"` | `"job"`), and `cpu_rate_percent = 0` (0 = no cap, else `1..=100`), with doc comments noting limits are OFF by default in Phase 1 (tight VRAM/commit budget per AGENTS.md) and are per-task overridable
    - Mirror the keys in `nebula-config` as `#[serde(default)] pub tasks: TasksConfig` with `TasksConfig { memory_limit_mib: u64, memory_limit_scope: MemoryScopeConfig, cpu_rate_percent: u8 }` (`#[serde(deny_unknown_fields)]`, `#[serde(default)]` fields, doc comments) and `enum MemoryScopeConfig { Process, #[default] Job }` (`#[serde(rename_all = "lowercase")]`); `NebulaConfig::validate` rejects `cpu_rate_percent > 100`
    - Document that a per-task override is carried as the `JobLimits` the executor (#32) passes to `TaskJob::new`; the physical-memory range check happens at `TaskJob::new` time (Requirement 7.2), not as a static config invariant
    - _Requirements: 7.6, 7.7, 8.2, 8.3_

  - [ ]* 10.2 Unit tests — config parse, defaults, and validation
    - `[tasks]` deserializes with defaults off (`memory_limit_mib = 0`, `cpu_rate_percent = 0`, scope `job`); an unknown key under `[tasks]` is rejected; `cpu_rate_percent > 100` fails `validate`; `memory_limit_scope = "process"` parses to `Process`
    - _Requirements: 7.6, 7.7, 8.2, 8.3_

- [ ] 11. Checkpoint — sandbox, model, and tools build and all tests pass
  - Confirm `cargo build -p nebula-sandbox -p nebula-model -p nebula-tools -p nebula-daemon` and `cargo nextest run --workspace --locked` pass; the issue #27 `shell.run`/`git.*` suites pass against the migrated seam, and model-server supervision is unchanged
  - Run the Windows-gated integration tests locally (and the `#[ignore]`d daemon-death test on the real machine) when the model is not needed
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 12. Documentation and final gate (protected set — never merge)
  - Add/refresh the `nebula-sandbox::job` and `task_job` module docs (the single audited unsafe surface; the per-task `TaskJob`, seam, and task-local), the thin notes in the migrated `nebula-model`/`nebula-tools` launchers, and `docs/ARCHITECTURE.md` (crate table + the Job-Object consolidation note) in the same change; note any `PHASE0_PLAN`/runbook touchpoints made stale
  - Run the full gate: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings` (which, with `unsafe` denied outside the one allowed module, enforces the single-unsafe-surface rule — Requirement 9.3); `cargo nextest run --workspace --locked`; confirm the bench `ruff` + `pytest` are unaffected
  - Open a pull request on a `feat/…` branch for human review and DO NOT merge it (protected-set feature, AGENTS.md hard rules 1 and 9); leave the `#[ignore]`d daemon-death test for the owner to run on the real machine
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marked with `*` are optional test/validation sub-tasks and can be skipped for a faster MVP, but all 10 property tests and the Windows-gated acceptance tests (no-survivors, assignment completeness, isolation, assign-before-escape, memory-limit reason, handle bookkeeping, only-task-processes, the source-scan, and the `#[ignore]`d daemon-death test) are strongly recommended given the safety-critical, protected-set nature of this feature.
- Each property test is tagged `// Feature: job-objects-for-tasks, Property N: ...` with a `Validates: Requirements X.Y` annotation. Decidable/structural properties (P8 mapping half, P9 range validation, P10 source scan) are `proptest`/unit tests with a minimum of 100 iterations where a generated input space applies; the properties that range over real OS behavior (P1 no-survivors, P2 assignment, P3 isolation, P4 daemon-death, P5 assign-before-escape, P6 handle bookkeeping, P7 only-task-processes, P8 real-OS half) are Windows-gated integration tests that generate process-tree shapes against real spawned processes; the `taskkill /F` daemon-death test (P4) is an `#[ignore]`d real-machine test per AGENTS.md.
- Property placement is next to the code it verifies: P10 with the consolidated module (tasks 1/6/8, scan in task 9), P8 with the completion port and `TaskJob` pump (tasks 2/4, real-OS half in task 9), P5 with the suspended-spawn seam (tasks 3/5, integration in task 9), P9 with `TaskJob::new` (task 4), and P1/P2/P3/P4/P6/P7 as integration tests in task 9 verifying the assembled seam and migration.
- Only coding, testing, and documentation tasks are included; no deployment, user-testing, or operational tasks. No test touches a real user path or the retired `C:` drive; integration-test files open with `#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]`.
- `unsafe` lives ONLY in `nebula-sandbox::job` with a `// SAFETY:` comment on every block; after this feature zero duplicate per-process task-scope job modules remain in `nebula-model`/`nebula-tools`.
- The two migration checkpoints (tasks 7 and 11) keep model-server supervision and issue #27's `shell.run`/`git.*` green across the consolidation; the final gate (task 12) runs the full gate and opens a PR without merging.
- Only processes a task started are ever affected — never the daemon, the `nebula-model` llama-server processes (their own daemon-lifetime per-process jobs), or the owner's processes (AGENTS.md hard rule 7).

## Task Dependency Graph

```json
{
  "waves": [
    { "id": 0, "tasks": ["1.1"] },
    { "id": 1, "tasks": ["1.2", "1.3"] },
    { "id": 2, "tasks": ["2.1", "3.1"] },
    { "id": 3, "tasks": ["2.2", "3.2", "4.1"] },
    { "id": 4, "tasks": ["4.2"] },
    { "id": 5, "tasks": ["4.3", "4.4", "5.1", "6.1", "10.1"] },
    { "id": 6, "tasks": ["5.2", "8.1", "10.2"] },
    { "id": 7, "tasks": ["5.3", "5.4", "8.2"] },
    { "id": 8, "tasks": ["8.3"] },
    { "id": 9, "tasks": ["9.1", "9.2", "9.3", "9.4", "9.5", "9.6", "9.7", "9.8", "9.9"] }
  ]
}
```
