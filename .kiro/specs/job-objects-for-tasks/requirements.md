# Requirements Document

## Introduction

This feature extends Nebula's Windows Job Object process supervision from a per-process scope to a per-task scope, so that every process a task starts — tool servers, `shell.run` children and their descendants, `git.*` children, and build processes — lives inside one kill-on-close Job Object owned by that task. Closing that single job (on task cancellation or daemon death) terminates the entire process tree, with no survivors. It implements GitHub issue #30 ("Job Objects for tool and build processes"), satisfies Phase 1 exit criterion 3, and carries the `core` label (part of the human-reviewed protected set).

Nebula is a local-first AI coding agent built as a Rust workspace targeting one Windows PC. Job Object supervision already exists in two independent places, each with the `KILL_ON_JOB_CLOSE` limit:

1. `crates/nebula-model/src/launcher.rs` — a private `job` module that wraps each llama-server process in its **own** job.
2. `crates/nebula-tools/src/launcher.rs` — a `pub(crate) mod job` plus a `pub(crate) mod child` (shipped in issue #27). `spawn_in_job(cmd) -> JobChild` places **one** child in a fresh kill-on-close job; `shell.run` and the `git.*` built-ins use it, and the external tool-server launcher (`StdioLauncher`) assigns each server to its own job.

### The problem this feature solves

Today each child process gets its **own** job (one process per job). This does not group a task's processes together, and a job holding only the direct child does **not** terminate grandchildren the child spawns (for example a shell that launches a build that launches a compiler). Issue #30 requires **one Job Object per task**: every process started on behalf of a task is assigned to that task's job, so closing the one job on task cancel or daemon death kills the whole tree — children and grandchildren alike.

### What this feature provides

1. A **Task_Job** abstraction — one Windows Job Object per Task, configured `KILL_ON_JOB_CLOSE`, optionally with a per-task Memory_Limit and CPU_Rate_Limit. The executor (issue #32) creates the Task_Job at task start and closes it at task end or cancellation.
2. A **per-task spawn seam** — the Task-scoped equivalent of `spawn_in_job`, so tool-server launch, `shell.run`, `git.*`, and build processes all spawn **into** the current Task's Task_Job rather than each creating a private job.
3. **Process-tree termination** — closing a Task_Job terminates every member process at any depth, including descendants spawned by `shell.run`.
4. **Termination-reason surfacing** — when a Memory_Limit (or CPU) breach causes a process or the job to be terminated, the reason is surfaced to the Task.

### Scope boundaries and cross-issue dependencies

- **The executor loop is issue #32.** #32 owns the Task lifecycle and decides *when* a Task_Job is created and closed. This feature provides the Task_Job and the spawn seam that #32 and the tool/build spawners call; it does not implement the executor.
- **The worktree is issue #29.** The Worktree and the Task_Job are both per-Task resources the executor manages; this feature owns only the job.
- **Circuit breakers and task-status reporting are issue #34.** This feature surfaces a termination reason when a limit is breached; it does not implement circuit-breaker policy. The global-stop time bound (2 seconds) referenced here is chosen to be consistent with issue #34.
- **Audited unsafe boundary.** The Win32 calls (`CreateJobObjectW`, `SetInformationJobObject` with `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` and CPU-rate control, `AssignProcessToJobObject`, kill-on-close) are the one audited `unsafe` surface. Per AGENTS.md, unsafe is denied outside isolated `win.rs`/`job` modules and every `unsafe` block needs a `// SAFETY:` comment.
- **Process rules from AGENTS.md apply.** Only processes a Task started are affected; the daemon's own processes and the owner's processes are never terminated by this mechanism (hard rule 7).

This is a **protected-set** feature. The primary risks are a process escaping its Task_Job (a survivor after cancel or daemon death), one Task's job termination affecting another Task or the daemon, and a handle leak that defeats kill-on-close. The requirements below are deliberately precise about those risks.

## Glossary

- **Nebula**: The local-first AI coding agent (Rust workspace, Windows target) that owns and runs Tasks.
- **Daemon**: The `nebula-daemon` process that supervises Nebula, holds Task_Job handles, and launches child processes on behalf of Tasks.
- **Task**: A unit of agent work driven by the executor loop (issue #32), identified by a `Task_Id`. Each Task owns exactly one Task_Job for its lifetime.
- **Task_Id**: The stable identifier of a Task, used to correlate a Task with its Task_Job and telemetry.
- **Task_Job**: One Windows Job Object created for a single Task, configured with Kill_On_Close and optionally a Memory_Limit and a CPU_Rate_Limit. Every process started on behalf of the Task is assigned to it.
- **Kill_On_Close**: The `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` behavior — when the last handle to a job closes (including on daemon crash), Windows terminates every process still assigned to that job.
- **Process_Tree**: The set of all processes started on behalf of a Task, at any depth — the direct children (tool servers, `shell.run` child, `git.*` child, build process) and all of their descendants (grandchildren and deeper).
- **Member_Process**: A process currently assigned to a given Task_Job.
- **Memory_Limit**: An optional per-Task cap on committed memory, enforced via `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` (`JOB_OBJECT_LIMIT_PROCESS_MEMORY` and/or `JOB_OBJECT_LIMIT_JOB_MEMORY`). A breach causes Windows to terminate the offending process or the job.
- **CPU_Rate_Limit**: An optional per-Task cap on CPU usage, enforced via `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION`.
- **Assign**: The act of placing a spawned process into a Task_Job via `AssignProcessToJobObject`, which must occur before the process can spawn descendants that escape the job.
- **Spawn_Seam**: The crate-internal API (the Task-scoped successor to `spawn_in_job` / `JobChild` from issue #27) through which tool-server launch, `shell.run`, `git.*`, and builds spawn a child into the current Task's Task_Job.
- **spawn_in_job / JobChild**: The existing issue #27 API in `crates/nebula-tools/src/launcher.rs` that spawns one child into a fresh per-process job and returns a guard whose drop closes the job. This feature replaces the per-process scope of this API with per-Task scope.
- **Survivor**: A process started on behalf of a Task that remains alive after that Task's Task_Job has been closed.
- **Audited_Unsafe_Boundary**: The single isolated module containing the `unsafe` Win32 FFI for job objects, where each `unsafe` block carries a `// SAFETY:` comment, per AGENTS.md.

## Requirements

### Requirement 1: One Job Object per task

**User Story:** As the Nebula executor, I want each task to own a single Job Object, so that all of the task's processes can be grouped and terminated together.

#### Acceptance Criteria

1. WHEN the executor starts a Task, THE Daemon SHALL create exactly one Task_Job for that Task, configured with Kill_On_Close.
2. WHEN the Daemon creates a Task_Job, THE Daemon SHALL associate that Task_Job with the Task's Task_Id such that every subsequent process spawned on behalf of that Task is assigned to that Task_Job.
3. THE Daemon SHALL create at most one Task_Job per live Task at any time.
4. IF Task_Job creation fails, THEN THE Daemon SHALL report a descriptive error indicating the Task_Job could not be created and SHALL NOT start any process on behalf of that Task.
5. WHEN the Daemon holds a Task_Job handle, THE Daemon SHALL retain that handle for the full lifetime of the Task so that the job is not closed prematurely while Member_Processes are still required.

### Requirement 2: Every task-started process is assigned to the task's job

**User Story:** As the owner, I want every process a task starts to be placed in that task's job, so that nothing the task launches can escape supervision.

#### Acceptance Criteria

1. WHEN the Daemon spawns any child process on behalf of a Task, THE Daemon SHALL assign that process to the Task_Job of the Task on whose behalf it is spawned, before the process is allowed to begin executing its intended work.
2. WHEN a tool server is launched on behalf of a Task, THE Daemon SHALL assign the tool-server process to that Task's Task_Job as the process on whose behalf it is spawned, independent of whether the same tool server is later shared with or reused by other Tasks.
3. WHEN a `shell.run` child or a `git.*` child is launched on behalf of a Task via the spawn-in-job seam, THE Daemon SHALL assign that child process to that Task's Task_Job at spawn time.
4. WHEN a build process is launched on behalf of a Task, THE Daemon SHALL assign the build process to that Task's Task_Job at spawn time.
5. THE Daemon SHALL assign every process it starts on behalf of a Task to exactly one Task_Job, namely that Task's Task_Job, such that no Member_Process is assigned to the Task_Job of a different Task and no Member_Process remains unassigned to any Task_Job.
6. IF assigning a spawned process to its Task's Task_Job fails, THEN THE Daemon SHALL, within 5 seconds of the failed assignment attempt, terminate that just-spawned process, report a descriptive error to the caller indicating that the Task_Job assignment failed, and SHALL NOT leave the process running outside the Task_Job.
7. IF a spawned process exits or is terminated before the Daemon can confirm its assignment to the Task's Task_Job, THEN THE Daemon SHALL report a descriptive error to the caller indicating that assignment could not be confirmed and SHALL NOT report the spawn as successful.

### Requirement 3: Grandchildren are terminated with the tree

**User Story:** As the owner, I want processes spawned by a task's children also terminated, so that a shell that launches a build leaves nothing behind.

#### Acceptance Criteria

1. WHEN a Member_Process of a Task_Job spawns a descendant process at any depth, THE descendant process SHALL become a Member_Process of the same Task_Job by inheritance of Job Object membership.
2. WHEN a Task_Job is closed, THE operating system SHALL terminate every Member_Process of that Task_Job at any depth, including children, grandchildren, and all deeper descendants spawned by a `shell.run` child, within 5 seconds of closure.
3. IF, after a Task_Job is closed, any Member_Process at any depth remains running longer than 5 seconds, THEN THE Daemon SHALL record the surviving process in telemetry and surface an error indication that the no-survivors property was violated, without terminating processes outside the Task_Job.
4. WHILE a Task_Job exists, THE Daemon SHALL configure the Task_Job so that breakaway is not permitted, such that a descendant that attempts to break away from the Task_Job remains a Member_Process of the Task_Job at any depth.

### Requirement 4: Assignment occurs before the process can escape

**User Story:** As a Nebula developer, I want each child assigned to the job before it can spawn descendants, so that no grandchild escapes the job during a spawn/assign race.

#### Acceptance Criteria

1. WHEN the Daemon spawns a process on behalf of a Task, THE Daemon SHALL assign that process to the Task's Task_Job before that process executes any user-controlled code capable of spawning a descendant process.
2. WHILE a spawned process has not yet been assigned to its Task's Task_Job, THE Daemon SHALL hold that process in a state in which it cannot execute user-controlled code that could spawn an unassigned descendant.
3. WHEN the Daemon spawns a process using a mechanism that holds the process from executing until released, THE Daemon SHALL assign the process to the Task_Job before releasing it to execute.
4. IF a spawned process is assigned to its Task_Job, THEN THE Daemon SHALL release that process to execute within 5 seconds of the spawn request.
5. IF the Daemon cannot guarantee that assignment to the Task_Job completes before the process executes user-controlled code, THEN THE Daemon SHALL terminate that process, SHALL return an error indicating that assignment ordering could not be guaranteed, and SHALL NOT leave the process running.
6. IF assignment of a spawned process to its Task_Job fails, THEN THE Daemon SHALL terminate that process within 5 seconds of the failure, SHALL return an error indicating that job assignment failed, and SHALL NOT leave the process or any of its descendants running.

### Requirement 5: Task cancellation terminates the whole tree

**User Story:** As the owner, I want cancelling a task to kill all of its processes quickly, so that cancelling leaves no survivors.

#### Acceptance Criteria

1. WHEN the executor cancels a Task, THE Daemon SHALL drop all duplicate handles to that Task's Task_Job and close the last handle so that the operating system terminates every Member_Process of the Process_Tree, including descendants nested at least 2 levels deep.
2. WHEN a Task's Task_Job is closed on cancellation, THE Daemon SHALL ensure that zero Survivor Member_Processes of that Task's Process_Tree remain alive 2 seconds after the cancellation request is received.
3. WHEN a Task completes normally, THE Daemon SHALL drop all duplicate handles to that Task's Task_Job and close the last handle so that any Member_Process still running is terminated within 2 seconds of job close.
4. IF one or more Member_Processes remain alive 2 seconds after the Task_Job's last handle is closed, THEN THE Daemon SHALL report an error identifying the Task_Id and indicating that termination did not complete, SHALL retain the count of surviving Member_Processes, and SHALL continue attempting termination of the remaining Member_Processes.
5. IF the Daemon cannot close the Task_Job because a duplicate handle cannot be dropped, THEN THE Daemon SHALL report an error identifying the Task_Id and indicating that the job could not be closed, and SHALL leave the Task's state unchanged.

### Requirement 6: Daemon death leaves no task processes

**User Story:** As the owner, I want killing the daemon to leave no task processes behind, so that a forced kill cannot leak running processes (Phase 1 exit criterion 3).

#### Acceptance Criteria

1. WHILE a Task is running, THE Daemon SHALL hold exactly one long-lived handle to that Task's Task_Job such that termination of the Daemon process closes that handle and leaves zero other open handles to the Task_Job.
2. WHEN the Daemon process is terminated by `taskkill /F`, THE operating system SHALL terminate every Member_Process of every Task_Job via Kill_On_Close within 10 seconds of the Daemon process exiting.
3. WHEN a spawned process tree is running under the Daemon and the Daemon is terminated by `taskkill /F`, THE operating system SHALL leave zero surviving Member_Processes of any Task_Job.
4. THE Daemon SHALL configure every Task_Job with Kill_On_Close before assigning any Member_Process to that Task_Job, such that no Task_Job depends on an orderly Daemon shutdown to terminate its Member_Processes.
5. WHEN the Daemon assigns a Member_Process to a Task_Job or spawns a child process, THE Daemon SHALL prevent the Task_Job handle from being inherited by that child process, such that the count of open Task_Job handles after the child is spawned equals the count before the child was spawned.
6. IF a Task_Job handle would be duplicated into a child process as an inheritable handle, THEN THE Daemon SHALL fail the spawn operation, terminate the Member_Processes already assigned to that Task_Job, and surface an error indicating that an inheritable job handle was prevented.

### Requirement 7: Optional per-task memory limit

**User Story:** As the owner, I want an optional memory cap per task, so that a runaway process is stopped and the task reports why.

#### Acceptance Criteria

1. WHERE a Memory_Limit is configured for a Task, THE Daemon SHALL apply that Memory_Limit to the Task's Task_Job at the time the Task_Job is created and before any Member_Process begins execution.
2. WHERE a Memory_Limit is configured for a Task, IF the configured Memory_Limit value is less than 1 byte or greater than the installed physical memory, THEN THE Daemon SHALL reject the Task before creating the Task_Job and surface an error to the caller indicating that the Memory_Limit is out of range, leaving no Task_Job created.
3. WHERE a Memory_Limit is configured with a per-process scope, WHILE the Memory_Limit is in effect for a Task_Job, IF the committed memory of a Member_Process exceeds the Memory_Limit, THEN THE operating system SHALL terminate that offending Member_Process.
4. WHERE a Memory_Limit is configured with a whole-job scope, WHILE the Memory_Limit is in effect for a Task_Job, IF the committed memory of the Task_Job exceeds the Memory_Limit, THEN THE operating system SHALL terminate the Task_Job.
5. WHEN a Member_Process or Task_Job is terminated because the Memory_Limit was exceeded, THE Daemon SHALL surface a termination reason to the Task within 5 seconds of the termination, indicating that the Memory_Limit was exceeded and reporting the Memory_Limit value and the scope that was applied.
6. WHERE no Memory_Limit is configured for a Task, THE Daemon SHALL create the Task_Job without a memory cap, such that memory usage is not constrained by this feature.
7. THE Daemon SHALL read any default Memory_Limit from configuration rather than from a hard-coded value.

### Requirement 8: Optional per-task CPU rate limit

**User Story:** As the owner, I want an optional CPU rate cap per task, so that a task cannot monopolize the CPU.

#### Acceptance Criteria

1. WHERE a CPU_Rate_Limit is configured for a Task, THE Daemon SHALL apply that CPU_Rate_Limit to the Task's Task_Job when the Task_Job is created.
2. WHERE no CPU_Rate_Limit is configured for a Task, THE Daemon SHALL create the Task_Job without a CPU rate cap.
3. THE Daemon SHALL read any default CPU_Rate_Limit from configuration rather than from a hard-coded value.

### Requirement 9: Audited unsafe Win32 boundary and module consolidation

**User Story:** As a Nebula maintainer, I want the job-object FFI isolated and consolidated, so that the audited unsafe surface lives in one reviewed place.

#### Acceptance Criteria

1. THE job-object Win32 FFI (`CreateJobObjectW`, `SetInformationJobObject`, `AssignProcessToJobObject`, and job-handle close) SHALL reside in a single isolated Audited_Unsafe_Boundary module, and no other module in the workspace SHALL declare these four job-object FFI calls.
2. THE Audited_Unsafe_Boundary module SHALL precede every `unsafe` block with a `// SAFETY:` comment, such that the count of `unsafe` blocks equals the count of `// SAFETY:` comments and each comment states the invariant relied upon.
3. IF any `unsafe` Win32 job-object call exists in library code outside the Audited_Unsafe_Boundary module, THEN THE workspace clippy lint run (`cargo clippy --workspace --all-targets --locked -- -D warnings`) SHALL fail with a diagnostic identifying the offending location.
4. THE feature SHALL expose exactly one shared Task_Job implementation used by tool-server launch, `shell.run`, `git.*`, and build spawning, such that zero duplicate per-process `job` modules for Task scope remain in `crates/nebula-model/src/launcher.rs` and `crates/nebula-tools/src/launcher.rs`.
5. WHERE library code outside the Audited_Unsafe_Boundary needs job-object behavior, THE library code SHALL obtain it through the shared Task_Job API and SHALL contain zero `unsafe` Win32 job calls.

### Requirement 10: Per-task job and spawn API for callers

**User Story:** As the executor and the tool/build spawners, I want a per-task job handle and spawn seam, so that I can create, spawn into, and close a task's job without touching Win32.

#### Acceptance Criteria

1. WHEN the executor requests a Task_Job for a Task_Id, THE Task_Job API SHALL return a handle that owns the Task_Job and whose closure terminates the Task_Job's Member_Processes via Kill_On_Close.
2. WHEN a caller spawns a process through the Spawn_Seam with a Task's Task_Job, THE Spawn_Seam SHALL spawn the process, assign it to that Task_Job before it can escape (per Requirement 4), and return a process handle to the caller.
3. WHEN the executor closes a Task_Job handle, THE Task_Job API SHALL terminate the Task_Job's Member_Processes and release the underlying job resources.
4. IF the Spawn_Seam is called without a valid Task_Job for the Task, THEN THE Spawn_Seam SHALL return a descriptive error and SHALL NOT spawn a process outside a Task_Job.
5. THE Task_Job API SHALL report errors as a single Tool-level error type defined in the library crate, consistent with the `thiserror` convention for libraries.

### Requirement 11: Only task-started processes are affected

**User Story:** As the owner, I want this mechanism to touch only task processes, so that the daemon and my own processes are never terminated.

#### Acceptance Criteria

1. WHEN a Task_Job is closed, THE operating system SHALL terminate only that Task_Job's Member_Processes and SHALL NOT terminate the Daemon process or any process not started on behalf of that Task.
2. WHEN a Task_Job for one Task is closed, THE Daemon SHALL leave every other live Task's Task_Job and Member_Processes unaffected.
3. THE Daemon SHALL NOT assign any process it did not start on behalf of a Task to any Task_Job.
4. THE Daemon SHALL NOT terminate, via this mechanism, the llama-server processes supervised by `nebula-model` or any process the owner started outside Nebula.

## Correctness Properties

These are candidate properties for the design phase to encode as property-based tests. They capture the three primary risks.

1. **No survivors (invariant).** FOR ALL Tasks and all Process_Trees of arbitrary depth, after the Task's Task_Job is closed, no process started on behalf of that Task SHALL remain alive. (Requirements 3, 5, 6)
2. **Assignment completeness (invariant).** FOR ALL processes spawned on behalf of a Task, each spawned process SHALL be a Member_Process of exactly that Task's Task_Job and of no other Task_Job. (Requirements 2, 4)
3. **Isolation (invariant).** FOR ALL pairs of distinct live Tasks, closing one Task's Task_Job SHALL NOT terminate any Member_Process of the other Task's Task_Job nor the Daemon process. (Requirement 11)

## Open Questions

1. **Job-module consolidation and home.** There are two per-process `job` modules today (`nebula-model` and `nebula-tools`). Should the shared Task_Job live in `nebula-tools`, or in a new dedicated crate the executor and both launchers depend on? Does `nebula-model`'s llama-server supervision also move to the shared implementation, or stay per-process (model servers are daemon-lifetime, not per-task)?
2. **Spawn/assign race mitigation.** Which mechanism guarantees assignment before a child can spawn an escaping grandchild: `CREATE_SUSPENDED` + `AssignProcessToJobObject` + resume, assign-immediately-after-spawn (accepting a narrow race window), or nested jobs? Note Windows nested-job rules and that the existing code uses `CREATE_NO_WINDOW` without `CREATE_SUSPENDED`.
3. **Tool-server lifetime vs. per-task job.** Issue #27's tool host launches tool servers once and may serve multiple Tasks across their lifetime. Do tool-server processes belong to a per-task Task_Job (and get killed when that one Task ends) or to a daemon-lifetime job? This is a real tension between #30's "one job per task" and #27's shared-host model and must be resolved in design.
4. **Memory-limit reason surfacing.** How is a Memory_Limit termination reported to the Task — as a completion-notification event, a Tool-level error, or a task-status field (ties into issue #34)? Which Windows signal (job notification, exit code, or completion port) is the source of truth that the limit, not an ordinary exit, caused termination?
5. **Default limits and configuration shape.** Are Memory_Limit and CPU_Rate_Limit per-task config keys, global defaults, or both? What are the defaults (if any) given the tight VRAM/commit-charge budget noted in AGENTS.md?
6. **Cancellation bound source.** The 2-second no-survivors bound is chosen to match issue #34's global-stop. Should it instead be configurable, and is the bound measured from job-close or from cancellation request?
