# Implementation Plan: Git Worktree Per Task (nebula-sandbox::worktree)

## Overview

This plan builds the per-task git worktree feature for Nebula (GitHub issue #29, design Section 4.4), the piece that replaces the single static `ConfigWorktreeRoot` wired by issue #27 with a **per-task confinement root** and adds the **`WorktreeManager`** lifecycle API (create / finalize / remove / recover) the executor loop (issue #32) will drive. It is a protected-set, data-sensitive feature (AGENTS.md hard rule 9), so tasks are strictly incremental and test-driven: the `worktree` module scaffold and config land first, then the git-shelling helper and the single `WorktreeError`, then `create` (branch naming, retired-drive rejection, per-repo serialization, partial-failure cleanup), then the per-task provider + task-local + sentinel, then the completion lifecycle, then the delete-limit gate over #28's approval contract, then crash recovery, then daemon wiring, then the property tests, then a checkpoint and a (never-merged) PR.

**Crate placement and the #28 dependency.** This feature lives in the `nebula-sandbox` crate created by issue #28 (the home for isolation and safety machinery) as a **new `worktree` module** alongside #28's permission engine. It **reuses #28's `Approval` / `ApprovalRequest` / `ApprovalStore`** for the delete-limit gate rather than inventing a second approval mechanism — the manager holds an `Arc<ApprovalStore>` and gates over-limit deletions on it exactly as the permission engine gates an above-threshold command. The task list assumes the `nebula-sandbox` crate and that approval contract already exist (#28 tasks 1 and 4); the worktree work is otherwise self-contained so it can be built in parallel with #28's later engine tasks. See task 1 for the explicit module/dependency note.

**Scope boundaries baked in.** Confinement is reused from the #27 `Path_Resolver`, not rebuilt here; the executor loop and `task.create` IPC are #32 (this feature only exposes the API #32 calls and shows, as a doc example, how #32 sets the task-local); push / PR / merge are #36 — this feature **creates the branch and worktree and keeps the branch**, performing no `git push`, no PR, no merge (AGENTS.md hard rule 1).

The design defines 10 correctness properties (P1–P10). Each is implemented as a single `proptest` property test (minimum 100 iterations where a generated input space applies), placed next to the code it verifies, and tagged `// Feature: git-worktree-per-task, Property N: ...` with a `Validates: Requirements X.Y` annotation. Tests build **real temporary git repositories** on tempdirs exactly as issue #27's `git.rs` tests do (`git init`, a committer identity, a seed commit).

Language: Rust (used throughout the design and the existing crates). Conventions enforced per task: `thiserror` in libraries, `anyhow` only in binaries, no `unwrap`/`expect`/`panic` in library code (clippy deny; test files carry the allow header), `unsafe` denied (none is needed here), doc comments on every public item and a module doc on each `lib.rs`, serde structs with `#[serde(deny_unknown_fields)]`, any new config key in BOTH `config/default.toml` and `nebula-config`, git child processes spawned with `kill_on_drop(true)` so a dropped future cannot leak a git process, and all tests redirect paths to tempdirs and never touch real paths or `C:`. Target: Windows MSVC.

> **Retired-drive sentinel caveat.** `NO_TASK_SENTINEL` is a `C:\...` path on purpose: it is the one path the `Path_Resolver` is guaranteed to reject (it runs the retired-drive check first). It is **never read from or written to** — it exists only to be handed to the resolver and rejected, satisfying the "fail closed" requirement without touching `C:` (AGENTS.md hard rule 5). Tests assert rejection; no test opens or creates it.

## Tasks

- [x] 1. Add the `worktree` module to `nebula-sandbox` and its config
  - [x] 1.1 Scaffold the `worktree` module and declare the #28 approval dependency
    - Create `crates/nebula-sandbox/src/worktree/mod.rs` and wire `pub mod worktree;` into `nebula-sandbox/src/lib.rs`; add a module doc describing the per-task worktree lifecycle, its protected-set status, and that it REUSES this crate's #28 `Approval`/`ApprovalRequest`/`ApprovalStore` for the delete-limit gate (no second approval mechanism)
    - Declare the empty submodule tree (`manager`, `provider`, `git`, `recovery`, `config`, `error`) to be filled by later tasks; add a dependency on the crate owning `path::resolve` (the `Path_Resolver` seam) if not already present via #28
    - Add `tokio` (process, sync, time, macros), `thiserror`, `serde` (derive), and `tracing` from `[workspace.dependencies]` to `nebula-sandbox` if not already present; add `proptest` and `tempfile` to `[dev-dependencies]`
    - This module depends ONLY on the #28 approval contract (`ApprovalStore`, `ApprovalRequest`, `Approval`) already defined by #28 task 4; it adds no new approval type
    - _Requirements: 7.6_
    - _Properties: P6_

  - [x] 1.2 Define `WorktreeConfig` and add the `[worktree]` config keys
    - Create `crates/nebula-sandbox/src/worktree/config.rs`; define `WorktreeConfig { worktrees_dir: PathBuf, branch_name_format: String }` with `#[serde(deny_unknown_fields)]`, `#[serde(default)]` on each field, and doc comments; defaults `worktrees_dir = F:\Nebula\worktrees` and `branch_name_format = "nebula/{task_id}"`
    - Implement `WorktreeConfig::validate(&self, retired_drive: &str) -> Result<(), WorktreeError>` rejecting a `branch_name_format` missing the `{task_id}` placeholder and a `worktrees_dir` on the retired drive
    - Add the `[worktree]` section (`worktrees_dir`, `branch_name_format`) to `config/default.toml` with doc comments, and mirror it in `nebula-config` as `pub worktree: WorktreeConfig` with `#[serde(default)]` and `deny_unknown_fields`; add `worktrees_dir` to `all_paths()` for the retired-drive doctor check
    - _Requirements: 8.1, 8.2, 8.3_
    - _Properties: P3_

  - [x] 1.3 Unit tests — config parse, defaults, and validation
    - `[worktree]` deserializes with defaults; an unknown key under `[worktree]` is rejected; a `branch_name_format` missing `{task_id}` and a `worktrees_dir` on the retired drive both fail `validate`; `worktrees_dir` appears in `all_paths()`
    - _Requirements: 8.1, 8.2, 8.3_

- [x] 2. Git-shelling helper and the single error type
  - [x] 2.1 Define `WorktreeError` (thiserror)
    - Create `crates/nebula-sandbox/src/worktree/error.rs`; define the single Tool-level `WorktreeError` enum with the design's variants: `TaskAlreadyActive`, `UnknownTask`, `NotARepository`, `UnresolvableRef`, `BranchExists`, `RetiredDrive`, `CreateFailed`, `DirtyWorktree`, `DeleteLimit { bytes, files }`, `ManualCleanupRequired { path }`, `CleanupFailed`, `Git`
    - Each variant carries a descriptive `thiserror` message; re-export from the module; no `unwrap`/`expect`/`panic`; doc comments on the public enum
    - _Requirements: 9.5_

  - [x] 2.2 Implement the internal `run_git` helper
    - Create `crates/nebula-sandbox/src/worktree/git.rs`; implement an internal `async fn run_git(repo_or_wt: &Path, args: &[&str], timeout: Duration) -> Result<GitOutput, WorktreeError>` mirroring `git.rs::run_git`: `tokio::process::Command::new("git")` with `current_dir` set, stdout/stderr piped, `kill_on_drop(true)`, bounded by `tokio::time::timeout`; map a non-zero exit or a spawn failure to `WorktreeError::Git`, a timeout to `WorktreeError::Git`
    - Add typed thin wrappers used by later tasks: `worktree_add(-b branch path start_point)`, `worktree_list_porcelain`, `worktree_remove(path)`, `worktree_prune`, `rev_parse_verify(ref)`, `branch_exists(branch)` (via `rev-parse --verify --quiet refs/heads/<branch>`), `status_porcelain(wt)`, `unmerged_commit_count(branch)` (via `rev-list --count <branch> --not --all`)
    - No `unwrap`/`expect`/`panic`; doc comments on every public item
    - _Requirements: 4.2, 9.5_
    - _Properties: P1_

  - [x] 2.3 Unit tests — run_git over a real temp repo
    - Over a seeded temp repo: `rev_parse_verify` resolves HEAD; `branch_exists` is false for an absent branch and true after creating one; a bogus ref maps to `WorktreeError::Git`/returns not-found; a non-repo dir maps to an error — no real path touched
    - _Requirements: 9.5_

- [x] 3. Worktree creation (`create`)
  - [x] 3.1 Implement `WorktreeManager` struct and `new`
    - Create `crates/nebula-sandbox/src/worktree/manager.rs`; define `WorktreeManager { config: WorktreeConfig, retired_drive: String, live: Arc<Mutex<HashMap<TaskId, Assignment>>>, repo_locks: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>, approvals: Arc<ApprovalStore> }` and `Assignment { root: PathBuf, branch: String }`
    - Implement `pub fn new(config: WorktreeConfig, retired_drive: String, approvals: Arc<ApprovalStore>) -> Self`; the `approvals` handle is the reused #28 store (task 1 note)
    - Doc comments on every public item; no `unwrap`/`expect`/`panic`
    - _Requirements: 9.1_

  - [x] 3.2 Implement `create` with branch naming, retired-drive rejection, and serialization
    - In `manager.rs`, implement `pub async fn create(&self, task_id: &TaskId, repo: &Path, start_point: Option<&str>) -> Result<PathBuf, WorktreeError>`: reject if `task_id` is already live (`TaskAlreadyActive`, Req 9.2) and if `repo` is not a git repository (`NotARepository`, Req 9.2)
    - Acquire the per-repo async creation lock so two simultaneous requests for the same repository cannot be assigned the same root or branch (`repo_locks`, Req 3.5); compute `path = worktrees_dir / task_id` (Req 1.3, 8.3) and `branch = branch_name_format.apply(task_id)` (default `nebula/<task-id>`, Req 1.4)
    - Check the resolved `worktrees_dir` and target path against `retired_drive` (same drive-letter comparison the `Path_Resolver` uses) BEFORE any git call → `RetiredDrive`, nothing created (Req 1.9, 8.2); reject if `refs/heads/<branch>` already exists → `BranchExists`, branch untouched, no worktree (Req 1.5)
    - Resolve the start-point: the supplied ref when given (unresolvable → `UnresolvableRef`, nothing created, Req 1.2) else the repository default branch HEAD (Req 1.1); run `git worktree add -b <branch> <path> <start-point>`; on success record `task_id → Assignment` and return the absolute `Worktree_Root` within 30 s (Req 1.6), release the lock
    - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.9, 3.1, 3.2, 3.5, 8.2, 8.3, 9.1, 9.2_
    - _Properties: P2, P3_

  - [x] 3.3 Implement partial-failure cleanup honoring the delete-limit
    - In `create`, when `git worktree add` fails after creating the directory or branch, measure the partial directory via the delete-limit measurement (task 6.1); within the limit → remove it, leave no live assignment, return `CreateFailed` (Req 1.7); over the limit → abort removal, retain the directory, return `ManualCleanupRequired { path }` (Req 1.8); on ANY create error no live assignment remains so a subsequent create for the same `task_id` is accepted (Req 9.6)
    - _Requirements: 1.7, 1.8, 9.6_
    - _Properties: P10_

  - [x] 3.4 Unit tests — create happy path, specified/unresolvable ref, branch collision, retired-drive
    - Over seeded temp repos: `create` returns an absolute root under `worktrees_dir` with the branch checked out (Req 9.1); create off a real tag succeeds and a bogus ref → `UnresolvableRef` creating nothing (Req 1.2); a pre-existing branch → `BranchExists`, branch unchanged, no worktree dir (Req 1.5); `worktrees_dir` on the tempdir's drive treated as retired → `RetiredDrive`, nothing created (Req 1.9, 8.2)
    - All paths redirected to tempdirs; retired drive set to an unused letter (e.g. `"Q:"`) except in the rejection test; never touch `C:`
    - _Requirements: 1.2, 1.5, 1.9, 8.2, 9.1_

- [x] 4. Per-task confinement root (provider, task-local, sentinel)
  - [x] 4.1 Implement `CURRENT_WORKTREE`, `NO_TASK_SENTINEL`, and `TaskWorktreeProvider`
    - Create `crates/nebula-sandbox/src/worktree/provider.rs`; declare `tokio::task_local! { pub static CURRENT_WORKTREE: Option<PathBuf>; }` with a doc comment stating the executor (#32) sets it via `CURRENT_WORKTREE.scope(Some(root), fut)` around a task's tool calls
    - Define `const NO_TASK_SENTINEL: &str = r"C:\nebula\no-task-in-scope"` with a doc comment noting it resolves on the retired drive so `path::resolve` rejects every candidate against it, and that no real worktree is ever placed there and nothing ever reads or writes it (sentinel caveat)
    - Define `#[derive(Clone, Copy, Debug, Default)] pub struct TaskWorktreeProvider;` with `pub fn new() -> Self`; implement `nebula_tools::WorktreeRootProvider` so `worktree_root()` reads `CURRENT_WORKTREE` — `Some(root)` → that root (Req 2.1, 2.3), `None`/unset → `NO_TASK_SENTINEL` (Req 2.5); keeps the infallible trait signature so no #27 call site changes
    - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5_
    - _Properties: P2, P4_

  - [x] 4.2 Property test — no task in scope yields a root the Path_Resolver rejects
    - **Property 4: No task in scope yields a root the Path_Resolver rejects**
    - **Validates: Requirements 2.5**
    - `proptest` over generated candidate paths: with no `CURRENT_WORKTREE` scope, assert `path::resolve(candidate, TaskWorktreeProvider::new().worktree_root(), retired_drive)` returns `Rejected`, so the originating built-in fails closed and no other task's root is returned; min 100 iterations; never read or write the sentinel path
    - Tag: `// Feature: git-worktree-per-task, Property 4: no task in scope yields a root the Path_Resolver rejects`
    - _Properties: P4_

  - [x] 4.3 Unit tests — provider resolves the scoped root; sentinel out of scope
    - Inside `CURRENT_WORKTREE.scope(Some(root), …)` the provider returns `root`; two concurrent `scope` tasks each read their own root and never the other's (Req 2.4); outside any scope the provider returns `NO_TASK_SENTINEL` and feeding it to `path::resolve` for any candidate yields `Rejected` (Req 2.5)
    - _Requirements: 2.3, 2.4, 2.5_

- [x] 5. Completion lifecycle (`finalize`, `remove_worktree`)
  - [x] 5.1 Implement `finalize`
    - In `manager.rs`, implement `pub async fn finalize(&self, task_id: &TaskId) -> Result<String, WorktreeError>`: retain the `Task_Branch` for #36 and return its name (Req 5.1, 9.3); do NOT remove the worktree directory (that is `remove_worktree`, Req 5.2) and do NOT delete the branch (Req 5.4); `UnknownTask` if `task_id` has no live assignment
    - _Requirements: 5.1, 5.2, 5.4, 9.3_
    - _Properties: P8_

  - [x] 5.2 Implement `remove_worktree` with the dirty-tree safety net and prune
    - In `manager.rs`, implement `pub async fn remove_worktree(&self, task_id: &TaskId) -> Result<(), WorktreeError>`: measure the directory and apply the delete-limit gate (task 6.1, Req 5.5); run `git worktree remove <path>` which REFUSES a dirty tree — map that to `DirtyWorktree`, deleting nothing (Req 5.6); on success run `git worktree prune` (Req 5.3), remove only that task's directory, keep the branch (Req 5.4); if removal or prune fails leave the branch and any unremoved registration intact and return `CleanupFailed` (Req 5.7)
    - Abandoned tasks are NOT auto-removed (settled decision): `remove_worktree` is called by #32 only after a PR is opened; abandoned worktrees are retained and surface in recovery
    - _Requirements: 5.2, 5.3, 5.4, 5.5, 5.6, 5.7_
    - _Properties: P5, P8_

  - [x] 5.3 Unit tests — dirty-tree refusal, branch retained, prune
    - Make a worktree dirty, call `remove_worktree` → `DirtyWorktree`, directory retained (Req 5.6); `finalize` returns the branch and it still resolves; `remove_worktree` on a clean worktree removes only that directory, runs prune, and leaves the branch resolvable (Req 5.1, 5.3, 5.4)
    - _Requirements: 5.1, 5.3, 5.4, 5.6_

- [x] 6. Delete-limit gate over the #28 approval contract
  - [x] 6.1 Implement delete-size measurement and the `Delete_Limit` gate
    - Create `crates/nebula-sandbox/src/worktree/measure.rs` (or in `manager.rs`); implement `async fn prepare_delete(&self, path: &Path) -> Result<DeleteDecision, WorktreeError>`: recursively sum the exact bytes and file count BEFORE any file is removed (Req 7.1); a measurement failure aborts and retains, returning an error (`CleanupFailed`/`CreateFailed`, Req 7.5)
    - Apply the exact thresholds: deletion proceeds without approval iff `bytes < 1_073_741_824` AND `files < 500` (Req 7.4); when `bytes >= 1_073_741_824` OR `files >= 500`, emit a #28 `ApprovalRequest` carrying the measured totals via the held `Arc<ApprovalStore>` and remove NOTHING until an authorizing #28 `Approval` is consumed (Req 7.2, 7.3), reusing the #28 mechanism — no new approval type (Req 7.6); surface `DeleteLimit { bytes, files }` to the caller while unapproved
    - Route every removal path (completion `remove_worktree`, recovery, partial-failure cleanup) through this one gate
    - _Requirements: 7.1, 7.2, 7.3, 7.4, 7.5, 7.6_
    - _Properties: P6, P7_

  - [x] 6.2 Property test — no single deletion exceeds the Delete_Limit without a recorded approval
    - **Property 6: No single deletion exceeds the Delete_Limit without a recorded approval**
    - **Validates: Requirements 1.8, 5.5, 6.7, 7.2, 7.3, 7.4, 7.6**
    - `proptest` over generated `(bytes, files)`: assert the gate deletes without approval exactly when strictly below both limits; when it meets or exceeds either limit, nothing is removed, a #28 `ApprovalRequest` is emitted, and all affected worktrees are retained until an authorizing `Approval` is consumed; min 100 iterations
    - Tag: `// Feature: git-worktree-per-task, Property 6: no single deletion exceeds the Delete_Limit without a recorded approval`
    - _Properties: P6_

  - [x] 6.3 Property test — delete-size measurement equals the actual tree totals
    - **Property 7: Delete-size measurement equals the actual tree totals**
    - **Validates: Requirements 7.1**
    - `proptest` building tempdir trees of known byte-size and file-count; assert the measured total bytes and file count equal the true totals; min 100 iterations; all paths in tempdirs, never `C:`
    - Tag: `// Feature: git-worktree-per-task, Property 7: delete-size measurement equals the actual tree totals`
    - _Properties: P7_

  - [x] 6.4 Unit test — delete-limit via a #28 approval
    - Force an over-limit measurement; assert a #28 `ApprovalRequest` is emitted, nothing is deleted, and consuming an authorizing `Approval` lets the deletion proceed; a measurement failure aborts and retains
    - _Requirements: 7.2, 7.5, 7.6_

- [x] 7. Crash recovery at daemon start (`recover_stale`)
  - [x] 7.1 Implement `RecoveryReport`, `StaleWorktree`, `Disposition`
    - Create `crates/nebula-sandbox/src/worktree/recovery.rs`; define `RecoveryReport { stale: Vec<StaleWorktree> }`, `StaleWorktree { root, branch, has_uncommitted_work, disposition }`, and `Disposition { Removed, RetainedForReview, AwaitingApproval { bytes, files } }` with doc comments
    - Implement `Uncommitted_Work` detection: `git status --porcelain` non-empty OR `unmerged_commit_count(branch) > 0` (commits on the `Task_Branch` reachable from no other ref, Req 6.4)
    - _Requirements: 6.3, 6.4_
    - _Properties: P5_

  - [x] 7.2 Implement `recover_stale`
    - In `manager.rs`, implement `pub async fn recover_stale(&self) -> RecoveryReport`: run `git worktree list --porcelain`, KEEP only entries whose path is under `worktrees_dir` (Req 6.2) — never delete, prune, or modify any worktree outside it; with no live task registry at cold start every such entry with no live task entry is a `Stale_Worktree` (Req 6.1)
    - For each candidate: dirty or carrying unmerged commits → `RetainedForReview`, never deleted (Req 6.5); indeterminate (unreadable registry or work-status) → `RetainedForReview`, never deleted (Req 6.8); clean and within the delete-limit → `git worktree remove` + prune, `Removed` (Req 6.6); clean but over the limit → emit a #28 `ApprovalRequest`, retain as `AwaitingApproval` until granted (Req 6.7, via task 6.1)
    - Report each stale worktree's root, branch, and `Uncommitted_Work` flag (Req 6.3, 9.4)
    - _Requirements: 6.1, 6.2, 6.5, 6.6, 6.7, 6.8, 9.4_
    - _Properties: P5, P9_

  - [x] 7.3 Property test — a worktree with uncommitted work is never deleted
    - **Property 5: A worktree with uncommitted work is never deleted**
    - **Validates: Requirements 5.6, 6.4, 6.5**
    - `proptest` generating clean / dirty / extra-commit worktrees over seeded temp repos; assert any cleanup path (completion removal or crash recovery) retains and reports any worktree whose tree is dirty OR whose `Task_Branch` carries commits reachable from no other ref, and never deletes it; min 100 iterations
    - Tag: `// Feature: git-worktree-per-task, Property 5: a worktree with uncommitted work is never deleted`
    - _Properties: P5_

  - [x] 7.4 Property test — crash recovery never touches worktrees outside Worktrees_Dir
    - **Property 9: Crash recovery never touches worktrees outside Worktrees_Dir**
    - **Validates: Requirements 6.1, 6.2**
    - `proptest` over a generated mix of worktrees inside and outside `worktrees_dir`; assert `recover_stale` deletes, prunes, or modifies only those under `worktrees_dir` and leaves every worktree outside it unchanged; min 100 iterations; all paths in tempdirs
    - Tag: `// Feature: git-worktree-per-task, Property 9: crash recovery never touches worktrees outside Worktrees_Dir`
    - _Properties: P9_

  - [x] 7.5 Unit test — recovery with a dirty worktree retains and reports
    - Register a stale dirty worktree and a clean one under `worktrees_dir`; run `recover_stale`; assert the dirty one is `RetainedForReview` and the clean one `Removed`, and that the report lists root/branch/flag for each; register a worktree OUTSIDE `worktrees_dir` and assert recovery never lists or touches it
    - _Requirements: 6.2, 6.3, 6.4, 6.5, 6.6_

- [x] 8. Daemon wiring
  - [x] 8.1 Replace `ConfigWorktreeRoot` with `TaskWorktreeProvider` and run recovery at start
    - In `nebula-daemon/src/builtin_providers.rs`, construct `WorktreeManager::new(config.worktree.clone(), config.resources.retired_drive.clone(), approvals)` once at daemon start (reusing the #28 `Arc<ApprovalStore>`), call `recover_stale().await` at start and log the result (`tracing::info!(event = "worktree.recovery", stale = …)`, Req 6/9.4)
    - In `build_tool_context`, install exactly one `Arc::new(TaskWorktreeProvider::new())` as `worktree` in place of `ConfigWorktreeRoot`, so no built-in call thereafter resolves through `ConfigWorktreeRoot` (Req 2.2); remove `ConfigWorktreeRoot` from the wiring path; leave `SamplerResourceProvider`/`NoResourceProvider` untouched
    - Add a doc comment (illustrative, NOT built here) showing how the future executor (#32) wraps a task's tool-calling future in `CURRENT_WORKTREE.scope(Some(root.clone()), …)` after `create`, and calls `finalize`/`remove_worktree` at task finalize
    - _Requirements: 2.2, 9.4_

  - [x] 8.2 Integration test — no-task-in-scope seam through the daemon
    - A `fs.*`/`shell.run` built-in called outside any `CURRENT_WORKTREE` scope fails with a confinement error (the sentinel integrates with the resolver); the provider installed by the daemon is the per-task one, not `ConfigWorktreeRoot`; all paths redirected to tempdirs
    - _Requirements: 2.2, 2.5_

- [x] 9. Checkpoint — sandbox, tools, and daemon build and all tests pass
  - Confirm `cargo build -p nebula-sandbox -p nebula-tools -p nebula-daemon` and `cargo nextest run --workspace --locked` pass; the issue #27 built-in tool suite passes unchanged against the swapped-in `TaskWorktreeProvider` (no #27 call site edited)
  - Ensure all tests pass, ask the user if questions arise.

- [x] 10. Correctness property tests (main-checkout, uniqueness, placement, lifecycle, recoverability)
  - [x] 10.1 Property test — the main checkout is never modified
    - **Property 1: The main checkout is never modified**
    - **Validates: Requirements 4.1, 4.2**
    - `proptest` generating random sequences of create/finalize/remove over a seeded temp repo; snapshot the `Main_Checkout`'s HEAD commit, checked-out branch name, and `git status --porcelain` before and after; assert all three are identical; min 100 iterations; all paths in tempdirs
    - Tag: `// Feature: git-worktree-per-task, Property 1: the main checkout is never modified`
    - _Properties: P1_

  - [x] 10.2 Property test — worktree and branch assignments are unique and non-overlapping across live tasks
    - **Property 2: Worktree and branch assignments are unique and non-overlapping across live tasks**
    - **Validates: Requirements 2.4, 3.1, 3.2, 3.3, 3.5, 4.4**
    - `proptest` generating sets of distinct task ids; create concurrently against one repo; assert pairwise-distinct `Worktree_Root`s and `Task_Branch`es, that concurrent same-repo requests never share a root or branch, and that `path::resolve` of a path under root A against root B is `Rejected(WorktreeEscape)`; min 100 iterations
    - Tag: `// Feature: git-worktree-per-task, Property 2: worktree and branch assignments are unique across live tasks`
    - _Properties: P2_

  - [x] 10.3 Property test — every created worktree is placed under Worktrees_Dir, never on the retired drive
    - **Property 3: Every created worktree is placed under the configured Worktrees_Dir, never on the retired drive**
    - **Validates: Requirements 1.3, 1.9, 8.1, 8.2, 8.3**
    - `proptest` over generated task ids and configs; assert a successfully created `Worktree_Root` is a descendant of `worktrees_dir`, and any config whose `worktrees_dir` or resulting path lies on the retired drive rejects creation with a retired-drive error creating nothing; min 100 iterations
    - Tag: `// Feature: git-worktree-per-task, Property 3: every created worktree is placed under Worktrees_Dir, never on the retired drive`
    - _Properties: P3_

  - [x] 10.4 Property test — finalize and remove retain the Task_Branch
    - **Property 8: Finalize and remove retain the Task_Branch**
    - **Validates: Requirements 5.1, 5.4**
    - `proptest` generating tasks; assert the `Task_Branch` resolves to the same commit after `finalize` and after `remove_worktree`, and that `remove_worktree` deletes only that task's worktree directory; min 100 iterations
    - Tag: `// Feature: git-worktree-per-task, Property 8: finalize and remove retain the Task_Branch`
    - _Properties: P8_

  - [x] 10.5 Property test — a failed create leaves no state blocking a retry
    - **Property 10: A failed create leaves no state blocking a retry**
    - **Validates: Requirements 1.7, 9.6**
    - `proptest` injecting create failures after partial on-disk/git state; assert no live assignment remains for that task id and a subsequent create for the same task id is accepted (not rejected as already active); min 100 iterations
    - Tag: `// Feature: git-worktree-per-task, Property 10: a failed create leaves no state blocking a retry`
    - _Properties: P10_

  - [x] 10.6 Integration tests — two concurrent tasks and main-checkout-untouched
    - Two concurrent `CURRENT_WORKTREE.scope(Some(root_i), …)` tasks against one repo: each reads its own root and never the other's, and the two worktrees and branches are distinct (Req 2.4, 3.1, 3.2); snapshot the main checkout's HEAD/branch/porcelain, run create → commit-in-worktree → finalize → remove, assert the snapshot is unchanged (Req 4.1)
    - _Requirements: 2.4, 3.1, 3.2, 4.1_

- [x] 11. Documentation and final gate (protected set — never merge)
  - Add/refresh the `nebula-sandbox::worktree` module doc and the module docs on `manager`, `provider`, `git`, `recovery`, `config`, and `error`; update the `nebula-daemon/src/builtin_providers.rs` module doc (ConfigWorktreeRoot → TaskWorktreeProvider) and `docs/ARCHITECTURE.md` (crate table + the per-task worktree note) in the same change
  - Run the full gate: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo nextest run --workspace --locked`; confirm the bench `ruff` + `pytest` are unaffected
  - Open a pull request on a `feat/…` branch for human review and DO NOT merge it (protected-set feature, AGENTS.md hard rules 1 and 9); the branch is kept for #36, and this feature performs no `git push`/PR/merge of task branches itself
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marked with `*` are optional test/validation sub-tasks and can be skipped for a faster MVP, but all 10 property tests and the acceptance unit/integration tests (two-concurrent-tasks isolation, crash recovery with a dirty worktree, main-checkout-untouched, sentinel rejection, delete-limit via #28 approval) are strongly recommended given the data-sensitive, protected-set nature of this feature.
- Each property test uses `proptest`, runs a minimum of 100 iterations, and is tagged with its `// Feature: git-worktree-per-task, Property N: ...` comment and a `Validates: Requirements X.Y` annotation.
- Property placement is next to the code it verifies: P1 with the git/create path, P2/P3 with create and the provider, P4 with the per-task provider (task 4), P5 with the uncommitted-work predicate and recovery, P6/P7 with the delete-limit gate (task 6), P8 with the completion lifecycle, P9 with recovery scope (task 7), and P10 with create recoverability; P1/P2/P3/P8/P10 and the concurrency/main-checkout integration tests are gathered in task 10 because they exercise the manager end-to-end over real temp repos.
- The `worktree` module lives in `nebula-sandbox` and REUSES #28's `Approval`/`ApprovalRequest`/`ApprovalStore` for the delete-limit gate; it defines no second approval mechanism. It assumes the `nebula-sandbox` crate (#28 task 1) and the approval contract (#28 task 4) already exist, and is otherwise self-contained so it can proceed in parallel with #28's later engine tasks.
- Confinement is reused from the #27 `Path_Resolver`, not rebuilt. The executor loop and `task.create` IPC are #32; push/PR/merge are #36 — this feature creates the branch and worktree and keeps the branch, performing no `git push`/PR/merge (AGENTS.md hard rule 1).
- Tests build real temp git repositories on tempdirs, redirect every path, and never touch real user paths or `C:`. `NO_TASK_SENTINEL` is a `C:\...` path only so the resolver rejects it; it is never read from or written to — tests assert rejection, never open it.
- The delete-limit (1 GiB / 500 files) is enforced in every cleanup path this feature writes (completion removal, crash recovery, partial-failure cleanup); only coding, testing, and documentation tasks are included; no deployment, user-testing, or operational tasks.
- Checkpoints (task 9) provide incremental validation; the final gate (task 11) runs the full gate and opens a PR without merging.

## Task Dependency Graph

```json
{
  "waves": [
    { "id": 0, "tasks": ["1.1"] },
    { "id": 1, "tasks": ["1.2", "2.1", "4.1"] },
    { "id": 2, "tasks": ["1.3", "2.2", "4.2", "4.3", "7.1"] },
    { "id": 3, "tasks": ["2.3", "3.1", "6.1"] },
    { "id": 4, "tasks": ["3.2", "6.2", "6.3", "6.4"] },
    { "id": 5, "tasks": ["3.3", "5.1", "5.2", "7.2"] },
    { "id": 6, "tasks": ["3.4", "5.3", "7.3", "7.4", "7.5"] },
    { "id": 7, "tasks": ["8.1"] },
    { "id": 8, "tasks": ["8.2", "10.1", "10.2", "10.3", "10.4", "10.5", "10.6"] }
  ]
}
```
