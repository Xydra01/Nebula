# Requirements Document

## Introduction

This feature gives every Nebula task its own git worktree and branch, so that concurrent tasks are isolated from one another and the owner's main checkout is never modified. It implements GitHub issue #29 ("Git worktree per task"), per design Section 4.4, and carries the `core` label (part of the human-reviewed protected set).

Nebula is a local-first AI coding agent built as a Rust workspace targeting one Windows PC. Issue #27 (merged) shipped the built-in tools (`fs.*`, `git.*`, `shell.run`) and a `WorktreeRootProvider` trait: everything the file and shell built-ins touch is confined to a single `worktree_root()` through the `Path_Resolver` (`crates/nebula-tools/src/path.rs`). The daemon currently wires a `ConfigWorktreeRoot` (`crates/nebula-daemon/src/builtin_providers.rs`) that returns one static `tools.builtin.worktree_root` from config for all tasks.

This feature **replaces that single static confinement root with a per-task worktree**. It supplies two things:

1. A **Worktree_Manager** — the lifecycle API (create, finalize, recover) that the executor loop (issue #32) calls. #32 drives *when* a worktree is created (at task start) and finalized; #29 owns *how*.
2. A **per-task `WorktreeRootProvider` implementation** that resolves to the worktree of the task currently executing, so that confinement — already implemented by the `Path_Resolver` for `fs.*` and `shell.run` — follows automatically. This feature does **not** redesign or reimplement path confinement; it only supplies the correct per-task root.

### Scope boundaries and cross-issue dependencies

- **Confinement is reused, not rebuilt.** The `Path_Resolver` already resolves every candidate path and rejects escapes from `worktree_root()` and any path on the `Retired_Drive`. This feature's responsibility ends at returning the correct per-task `Worktree_Root`; the resolver's behavior is out of scope (owned by issue #27).
- **The executor loop is issue #32.** Task creation and the `task.create` IPC method live in #32. This feature exposes the Worktree_Manager API that #32 calls at task start and task finalization; it does not implement the executor or the IPC surface.
- **Push and PR creation are issue #36.** This feature creates the branch and the worktree and **keeps the branch** on completion so #36 can open the PR. This feature performs no `git push`, no PR creation, and no merge (AGENTS.md hard rule 1).
- **Secrets, drives, and process rules** from AGENTS.md apply: worktrees live under the configured `Worktrees_Dir` on the hot drive, nothing resolves to the `Retired_Drive`, cleanup honors the `Delete_Limit`, and only processes this feature started are stopped.

This is a **protected-set, data-sensitive** feature. The primary risks are modifying the owner's main checkout, two tasks colliding on one worktree, and cleanup silently deleting uncommitted work. The requirements below are deliberately precise about those three risks.

## Glossary

- **Nebula**: The local-first AI coding agent (Rust workspace, Windows target) that owns and runs tasks.
- **Task**: A unit of agent work driven by the executor loop (issue #32), identified by a `Task_Id`. Each Task runs confined to its own Worktree.
- **Task_Id**: The stable identifier of a Task, inherited from the executor and used to correlate a Task with its Worktree and telemetry.
- **Worktree_Manager**: The lifecycle API this feature provides for creating, finalizing, and recovering Worktrees. The executor (issue #32) calls it; it is the clean integration seam between the executor and git worktree management.
- **Worktree**: A git working tree created by `git worktree add`, with its own checked-out `Task_Branch`, located under the `Worktrees_Dir`. A Worktree shares the repository object store with the `Main_Checkout` but has an independent working directory.
- **Worktree_Root**: The absolute directory path of a Task's Worktree; the single directory to which that Task's `fs.*` and `shell.run` operations are confined via the `Path_Resolver`.
- **WorktreeRootProvider**: The trait defined in issue #27 (`crates/nebula-tools/src/builtins/mod.rs`) whose `worktree_root()` returns the current confinement root. This feature supplies a per-task implementation that resolves to the executing Task's `Worktree_Root`, replacing the static `ConfigWorktreeRoot`.
- **Task_Branch**: The git branch created for a Task and checked out in its Worktree, named by the configured branch-naming scheme (for example `nebula/<task-id>`). The branch is retained after the Task completes so issue #36 can open a pull request from it.
- **Main_Checkout**: The owner's primary working tree of a repository — the repository the Task's Worktree is derived from. It is never modified by any Task operation.
- **Worktrees_Dir**: The configured parent directory for all Worktrees (`F:\Nebula\worktrees\` by default), resolved from configuration on the hot drive. Not hard-coded.
- **Stale_Worktree**: A Worktree that remains registered in git after the daemon stops or crashes, with no live Task owning it. Detected at daemon start via the git worktree registry.
- **Uncommitted_Work**: Changes in a Worktree that are not safely preserved in git — a non-clean working tree (per `git status --porcelain`) and/or commits on the `Task_Branch` that exist nowhere else. The precise detection basis is an open question (see Open Questions).
- **Delete_Limit**: The AGENTS.md hard-rule-4 threshold on deletions: any deletion exceeding 1 GB or 500 files requires explicit owner approval. Cleanup code this feature writes stops at this limit.
- **Retired_Drive**: The drive designated by the `resources.retired_drive` configuration value (default `"C:"`), the failing boot drive, which must never be read from or written to (AGENTS.md hard rule 5).
- **Daemon**: The `nebula-daemon` process that supervises Nebula and, at start, runs Stale_Worktree recovery.

## Requirements

### Requirement 1: Create a worktree and branch at task start

**User Story:** As the Nebula executor, I want a dedicated worktree and branch created when a task starts, so that the task runs in isolation on its own branch.

#### Acceptance Criteria

1. WHEN the executor requests Worktree creation for a Task against a target repository without specifying a start-point ref, THE Worktree_Manager SHALL create a new git Worktree with a new Task_Branch based on the current HEAD commit of the target repository's default branch, using `git worktree add` so that the Main_Checkout working tree is not modified.
2. WHEN the executor requests Worktree creation for a Task and specifies a start-point ref, THE Worktree_Manager SHALL base the new Task_Branch on that specified ref, and IF the specified ref cannot be resolved in the target repository, THEN THE Worktree_Manager SHALL reject the creation and return a descriptive error indicating the ref could not be resolved, without creating the Worktree.
3. WHEN the Worktree_Manager creates a Worktree, THE Worktree_Manager SHALL place the Worktree directory under the configured Worktrees_Dir.
4. WHEN the Worktree_Manager creates a Task_Branch, THE Worktree_Manager SHALL name the branch using the configured branch-naming scheme and SHALL check that branch out in the created Worktree.
5. IF a branch with the resolved Task_Branch name already exists in the target repository, THEN THE Worktree_Manager SHALL reject the creation, return a descriptive error indicating the branch name is already in use, and SHALL NOT create the Worktree or modify the existing branch.
6. WHEN Worktree creation succeeds, THE Worktree_Manager SHALL return the absolute Worktree_Root of the created Worktree to the caller within 30 seconds of the creation request.
7. IF Worktree creation fails after the Worktree directory or Task_Branch was partially created, THEN THE Worktree_Manager SHALL remove the partially created Worktree directory, return a descriptive error indicating the creation failed, and leave no Worktree registered for that Task, provided the removal honors the delete-safety limit of 1 GB or 500 files.
8. IF partial-failure cleanup would exceed the delete-safety limit of 1 GB or 500 files, THEN THE Worktree_Manager SHALL abort the automatic removal, retain the partially created directory, and return a descriptive error indicating that manual cleanup is required.
9. IF a resolved Worktree_Root or any path used for Worktree creation resolves to the Retired_Drive, THEN THE Worktree_Manager SHALL reject the creation, return a descriptive error indicating the path is on a prohibited drive, and SHALL NOT create the Worktree.

### Requirement 2: Per-task confinement root

**User Story:** As a Nebula developer, I want the built-in tools confined to the current task's worktree, so that confinement follows per-task without changing the tools.

#### Acceptance Criteria

1. THE Worktree_Manager SHALL provide a WorktreeRootProvider implementation whose `worktree_root()` returns the Worktree_Root of the Task currently executing.
2. WHEN the daemon wires the built-in tool context, THE Daemon SHALL install exactly one per-task WorktreeRootProvider instance in place of the static ConfigWorktreeRoot, such that no built-in tool call thereafter resolves its root through ConfigWorktreeRoot.
3. WHILE a Task is executing, THE per-task WorktreeRootProvider SHALL resolve `worktree_root()` to that Task's Worktree_Root within 50 milliseconds of the call and SHALL return a path identical to the Worktree_Root recorded for that Task at Task creation.
4. WHERE 2 or more Tasks (up to the configured concurrent-task maximum) execute concurrently through a single shared WorktreeRootProvider, THE per-task WorktreeRootProvider SHALL resolve `worktree_root()` to the Worktree_Root of the Task on whose behalf the current built-in call is made, keyed by the current Task identity, and SHALL NOT resolve to the Worktree_Root of any other concurrently executing Task.
5. IF a built-in call is made with no Task currently in scope, THEN THE per-task WorktreeRootProvider SHALL resolve to a sentinel root that the Path_Resolver rejects, SHALL cause the originating built-in call to fail with an error indicating that no active Task worktree is in scope, SHALL NOT return a Worktree_Root belonging to any other Task, and SHALL leave the state of all Tasks' worktrees unchanged.

### Requirement 3: Concurrent-task isolation

**User Story:** As the owner, I want two tasks on the same repository to run in separate worktrees, so that they never interfere with each other.

#### Acceptance Criteria

1. WHEN two or more Tasks run against the same repository at the same time, THE Worktree_Manager SHALL assign each Task a distinct Worktree whose Worktree_Root is a distinct absolute path, such that no two concurrently live Tasks share a Worktree_Root.
2. WHEN two or more Tasks run against the same repository at the same time, THE Worktree_Manager SHALL assign each Task a distinct Task_Branch, such that no branch name is assigned to more than one concurrently live Task.
3. WHILE two or more Tasks execute concurrently, THE Worktree_Manager SHALL confine each Task's file and shell operations to that Task's own Worktree_Root, such that any operation whose resolved target path lies outside that Task's Worktree_Root is rejected with a descriptive error and performs no read or write.
4. IF a Worktree creation request would resolve to a Worktree_Root or Task_Branch already assigned to a live Task, THEN THE Worktree_Manager SHALL reject the request, return a descriptive error indicating the conflict, SHALL NOT reuse or modify the existing Worktree, and SHALL leave the existing live Task's Worktree and Task_Branch unchanged.
5. WHILE a Worktree creation request is being processed, THE Worktree_Manager SHALL serialize concurrent creation requests for the same repository so that at most one Worktree_Root and Task_Branch assignment is computed and committed at a time, such that two simultaneous requests cannot be assigned the same Worktree_Root or Task_Branch.

### Requirement 4: Main checkout is never modified

**User Story:** As the owner, I want my main checkout left untouched by any task, so that my working copy is safe while the agent works.

#### Acceptance Criteria

1. WHILE any Task executes, THE Worktree_Manager SHALL perform all Task working-tree changes within that Task's Worktree, such that the Main_Checkout's HEAD commit, checked-out branch name, and `git status --porcelain` output are identical before and after the Task.
2. WHEN the Worktree_Manager creates, finalizes, or recovers a Worktree, THE Worktree_Manager SHALL operate through git worktree and branch operations that do not alter the Main_Checkout working tree or its checked-out branch.
3. IF a requested operation would modify the Main_Checkout working tree or check out the Task_Branch into the Main_Checkout, THEN THE Worktree_Manager SHALL reject the operation before any filesystem change and return a descriptive error, leaving the Main_Checkout unchanged.
4. IF a Task operation targets the Main_Checkout working tree by an absolute path, a symlink, or a `..` traversal out of the Worktree_Root, THEN confinement via the Path_Resolver SHALL reject that operation so that the Main_Checkout is not written.

### Requirement 5: Completion lifecycle

**User Story:** As the owner, I want a finished task's worktree cleaned up while its branch is kept, so that the pull request can still be opened and disk space is reclaimed.

#### Acceptance Criteria

1. WHEN the executor finalizes a Task, THE Worktree_Manager SHALL retain the Task_Branch so that issue #36 can open a pull request from it.
2. WHEN a Task reaches a terminal state (its pull request has been opened, or the Task has been abandoned by the executor), THE Worktree_Manager SHALL remove that Task's Worktree directory as a separate operation from finalization.
3. WHEN the Worktree_Manager removes a Worktree directory, THE Worktree_Manager SHALL run `git worktree prune` after the directory removal completes, so that the removed Worktree's registration is cleared from git.
4. WHEN the Worktree_Manager removes a Worktree, THE Worktree_Manager SHALL remove only that Task's Worktree directory and SHALL NOT delete or modify the Task_Branch.
5. WHERE removing a Worktree directory would exceed the Delete_Limit, THE Worktree_Manager SHALL stop at the Delete_Limit and request explicit owner approval before deleting, as defined in Requirement 7.
6. IF Worktree removal is requested for a Worktree that contains Uncommitted_Work, THEN THE Worktree_Manager SHALL apply the Uncommitted_Work protection defined in Requirement 6 and SHALL rely on `git worktree remove` refusing to remove a dirty Worktree rather than deleting silently.
7. IF Worktree directory removal or `git worktree prune` fails, THEN THE Worktree_Manager SHALL leave the Task_Branch and any unremoved Worktree registration intact and SHALL report an error indicating that cleanup did not complete.

### Requirement 6: Crash-recovery detection and reporting at daemon start

**User Story:** As the owner, I want leftover worktrees from a crash detected and reported at startup, so that uncommitted work is never silently discarded.

#### Acceptance Criteria

1. WHEN the Daemon starts, THE Worktree_Manager SHALL enumerate registered Worktrees located under the Worktrees_Dir from the git worktree registry and identify each as a Stale_Worktree when it has no corresponding live Task entry in the Task registry.
2. WHILE performing Stale_Worktree recovery, THE Worktree_Manager SHALL NOT delete, prune, or modify any worktree located outside the Worktrees_Dir.
3. WHEN the Worktree_Manager detects one or more Stale_Worktrees, THE Worktree_Manager SHALL produce a startup report listing, for each detected Stale_Worktree, its Worktree_Root, its Task_Branch, and whether it contains Uncommitted_Work.
4. WHEN the Worktree_Manager evaluates a Stale_Worktree for cleanup, THE Worktree_Manager SHALL determine it contains Uncommitted_Work if the Worktree has tracked modifications, staged changes, or non-ignored untracked files, or commits on its Task_Branch that exist nowhere else.
5. IF a Stale_Worktree contains Uncommitted_Work, THEN THE Worktree_Manager SHALL retain that Stale_Worktree and report it for owner review, and SHALL NOT delete it.
6. WHEN a Stale_Worktree contains no Uncommitted_Work and its removal is within the Delete_Limit, THE Worktree_Manager SHALL remove that Stale_Worktree and run `git worktree prune`.
7. WHILE performing Stale_Worktree recovery, THE Worktree_Manager SHALL stop any removal that would exceed the Delete_Limit and request explicit owner approval, as defined in Requirement 7.
8. IF the git worktree registry cannot be read or a Stale_Worktree's Uncommitted_Work status cannot be determined, THEN THE Worktree_Manager SHALL retain the affected Worktree, report it for owner review, and SHALL NOT delete it.

### Requirement 7: Delete-limit compliance in cleanup

**User Story:** As the owner, I want all worktree cleanup to respect the delete limit, so that no large deletion happens without my approval.

#### Acceptance Criteria

1. WHEN the Worktree_Manager prepares to delete one or more Worktrees, THE Worktree_Manager SHALL compute, before any file is removed, the total size in bytes and the total file count by recursively traversing each target Worktree directory and summing the sizes and counts of all contained files.
2. IF a prepared deletion's total size is greater than or equal to 1073741824 bytes (1 GB) OR its total file count is greater than or equal to 500 files, THEN THE Worktree_Manager SHALL stop before removing any file, report the measured total size in bytes and total file count to the owner, request explicit owner approval, and SHALL NOT remove any file until approval is explicitly granted.
3. WHILE a deletion that meets or exceeds the Delete_Limit (1073741824 bytes or 500 files) has not received explicit owner approval, THE Worktree_Manager SHALL retain all affected Worktrees unchanged.
4. WHEN a prepared deletion's total size is less than 1073741824 bytes AND its total file count is less than 500 files, THE Worktree_Manager SHALL perform the deletion without requesting owner approval.
5. IF measurement of a target Worktree directory cannot be completed before deletion, THEN THE Worktree_Manager SHALL abort the deletion, report an error indicating the measurement failed, and SHALL retain all affected Worktrees unchanged.
6. WHERE the owner-approval mechanism defined by the permission-tiers feature is available, THE Worktree_Manager SHALL obtain Delete_Limit approval through that mechanism rather than through a separate approval gate.

### Requirement 8: Retired-drive and path safety

**User Story:** As the owner, I want worktrees kept off the failing C: drive and on configured paths, so that the retired drive is never touched and paths are not hard-coded.

#### Acceptance Criteria

1. THE Worktree_Manager SHALL resolve the Worktrees_Dir and all Worktree paths from configuration values, and SHALL NOT use hard-coded drive paths.
2. IF any resolved Worktree path lies on the Retired_Drive, THEN THE Worktree_Manager SHALL reject the operation and return a descriptive error.
3. WHEN the Worktree_Manager creates a Worktree, THE Worktree_Manager SHALL place the Worktree on the configured hot-drive Worktrees_Dir.

### Requirement 9: Executor integration seam

**User Story:** As the Nebula executor (issue #32), I want a clean worktree-manager API, so that I can drive the worktree lifecycle without owning git details.

#### Acceptance Criteria

1. WHEN the executor calls the create operation with a Task_Id and a target repository path, THE Worktree_Manager SHALL create a Worktree and a Task_Branch and return a success result carrying the Worktree_Root.
2. IF the create operation is called with a Task_Id that already has an active Worktree, or with a target repository path that does not resolve to an existing git repository, THEN THE Worktree_Manager SHALL return a Tool-level error indicating the specific failure cause and SHALL NOT create a new Worktree or Task_Branch.
3. WHEN the executor calls the finalize operation for a Task, THE Worktree_Manager SHALL retain the Task_Branch and remove the Worktree directory, subject to Requirements 5, 6, and 7, and SHALL return a success result carrying the retained Task_Branch.
4. WHEN the Daemon starts, THE Worktree_Manager SHALL perform Stale_Worktree detection and return a report enumerating each detected Stale_Worktree, subject to Requirement 6.
5. WHEN the executor calls any Worktree_Manager operation, THE Worktree_Manager SHALL return a result that is either a success result carrying the relevant Worktree_Root or Task_Branch, or a single Tool-level error value defined in the library crate, such that the executor issues no git worktree commands directly.
6. IF a Worktree_Manager operation fails after partially modifying on-disk or git state, THEN THE Worktree_Manager SHALL return a Tool-level error indicating the failure cause and SHALL leave no Worktree directory or Task_Branch that a subsequent create operation for the same Task_Id would reject as already active.

## Correctness Properties

These are candidate properties for the design phase to encode as property-based tests. They capture the three primary risks.

1. **Main checkout invariant (invariant).** FOR ALL sequences of Task operations, the Main_Checkout working tree and its checked-out branch SHALL be unchanged after the operations complete. (Requirement 4)
2. **Worktree path uniqueness (invariant).** FOR ALL pairs of distinct live Tasks, their resolved Worktree_Roots SHALL be distinct, and no path confined to one Task's Worktree_Root SHALL resolve into another's. (Requirements 2, 3)
3. **Safe cleanup (invariant / metamorphic).** FOR ALL cleanup operations, a Worktree containing Uncommitted_Work SHALL never be deleted, and the total of any single deletion SHALL never exceed the Delete_Limit without recorded owner approval. (Requirements 5, 6, 7)

## Open Questions

1. **Branch-naming scheme.** Is the Task_Branch named `feat/<task>`, `nebula/<task-id>`, or configurable? Design 4.4 shows both `feat/<task>` and `nebula/<task-id>`.
2. **Worktree directory naming.** Is the Worktree directory named by `Task_Id` or by sanitized `Task_Branch`? This affects collision avoidance and readability.
3. **Uncommitted_Work detection basis.** Does detection use `git status --porcelain` (dirty working tree), unpushed/unmerged commits on the Task_Branch, or both? The design says "uncommitted/un-pushed"; the exact combination needs to be fixed.
4. **Abandoned-task worktrees.** Are abandoned Tasks' Worktrees removed immediately (like PR-opened Tasks) or kept for owner inspection for some period? Issue scope says "remove after PR opened or task abandoned," but inspection value may argue for retention.
5. **Per-task root selection mechanism.** How is the "current Task" determined inside the WorktreeRootProvider — keyed by a Task_Id / Trace_Id carried in task-local or async-context state? This determines how concurrent calls disambiguate (Requirement 2.4).
   - **No-task-in-scope handling.** The existing `WorktreeRootProvider::worktree_root(&self) -> PathBuf` is infallible. Requirement 2.5 currently resolves this by returning a sentinel path that the Path_Resolver rejects. The design must decide between keeping that rejected-sentinel approach vs. changing the trait signature to be fallible (e.g. returning a `Result`), which would affect the #27 call sites that consume this trait.
6. **Prune scope.** Does `git worktree prune` run per-removal, or once per recovery pass, to avoid racing concurrent live Tasks' registrations?
