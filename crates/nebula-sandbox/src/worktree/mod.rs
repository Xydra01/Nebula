//! Per-task git worktree lifecycle (GitHub issue #29, design Section 4.4).
//!
//! This module gives every Nebula task its own git worktree and branch, so concurrent tasks are
//! isolated from one another and the owner's `Main_Checkout` is never modified. It replaces the
//! single static `ConfigWorktreeRoot` that issue #27 wired into the daemon with a **per-task
//! confinement root**, and it adds the `WorktreeManager` lifecycle API (create / finalize /
//! remove / recover) the executor loop (issue #32) will drive.
//!
//! # Lifecycle
//!
//! - **create** — `git worktree add -b <branch> <path> <start-point>` places a worktree at
//!   `Worktrees_Dir/<task_id>` on the hot drive and checks out a new `Task_Branch` (default
//!   `nebula/<task-id>`). The `Main_Checkout` working tree is never touched.
//! - **confine** — a `nebula_tools::TaskWorktreeProvider` reads the executing task's root from a
//!   `tokio` task-local so the `nebula-tools` `Path_Resolver` confines every `fs.*` / `shell.run`
//!   call to that task's worktree automatically. No task in scope resolves to a rejected sentinel.
//!   (The provider lives in `nebula-tools`, not here — see the [`provider`] stub for why.)
//! - **finalize** — retains the `Task_Branch` for issue #36's pull-request flow; it performs no
//!   `git push`, no PR creation, and no merge (AGENTS.md hard rule 1).
//! - **remove** — removes only that task's worktree directory (`git worktree remove`, which
//!   refuses a dirty tree) and prunes the registration, keeping the branch.
//! - **recover** — at daemon start, detects `Stale_Worktree`s left under `Worktrees_Dir` by a
//!   crash, reports uncommitted work, and never deletes a worktree carrying work.
//!
//! # Protected set
//!
//! This is a protected-set, data-sensitive feature (AGENTS.md hard rule 9). The three risks that
//! drive every requirement are: the owner's `Main_Checkout` being modified, two tasks colliding on
//! one worktree, and cleanup silently discarding uncommitted work or over-deleting. Changes here
//! are human-reviewed and kept small, and no path ever resolves to the retired drive.
//!
//! # Reused #28 approval contract (no second approval mechanism)
//!
//! Cleanup must honour the AGENTS.md delete-limit (1 GiB / 500 files). This module **reuses this
//! crate's issue #28 approval contract** — [`Approval`](crate::Approval),
//! [`ApprovalRequest`](crate::approval::ApprovalRequest), and
//! [`ApprovalStore`](crate::approval::ApprovalStore) — to gate an over-limit deletion, exactly as
//! the permission engine gates an above-threshold command. The `WorktreeManager` holds an
//! `Arc<ApprovalStore>`, emits an `ApprovalRequest` carrying the measured byte/file totals when a
//! deletion would meet or exceed the limit, and removes nothing until an authorizing `Approval` is
//! consumed. It defines **no second approval type** of its own.
//!
//! (The `WorktreeManager`, `TaskWorktreeProvider`, and the other types named above are declared in
//! the submodules below and implemented by later tasks of this feature.)
//!
//! # Scope boundaries
//!
//! Path confinement itself is **reused, not rebuilt**: it belongs to the issue #27 `Path_Resolver`
//! (`nebula_tools::path::resolve`), the `Path_Resolver` seam this module feeds the correct per-task
//! root. The executor loop and the `task.create` IPC method are issue #32; push / PR / merge are
//! issue #36. This feature creates the branch and worktree and keeps the branch — nothing more.
//!
//! # Module tour
//!
//! The submodules below are declared here and filled by later tasks of the feature:
//!
//! - [`config`] — `WorktreeConfig` and the `[worktree]` config keys (`worktrees_dir`,
//!   `branch_name_format`) with their validation.
//! - [`error`] — the single Tool-level `WorktreeError` returned by every manager operation.
//! - [`git`] — the internal `run_git` helper and the typed thin wrappers over the `git worktree`
//!   subcommands (`add` / `list` / `remove` / `prune`) and the ref/branch/status queries.
//! - [`manager`] — the `WorktreeManager` struct and its `create` / `finalize` / `remove_worktree` /
//!   `recover_stale` lifecycle methods.
//! - [`measure`] — delete-size measurement and the `Delete_Limit` gate (1 GiB / 500 files) over the
//!   #28 approval contract that every removal path funnels through.
//! - [`provider`] — a documentation stub. The per-task provider seam (`CURRENT_WORKTREE`
//!   task-local, no-task-in-scope sentinel, and `TaskWorktreeProvider`) is **relocated to
//!   `nebula-tools`** (`nebula_tools::TaskWorktreeProvider`) because it must implement
//!   `nebula_tools::WorktreeRootProvider`, and this crate must not depend on `nebula-tools`
//!   (cycle). See [`provider`] for the full note.
//! - [`recovery`] — the `RecoveryReport` / `StaleWorktree` / `Disposition` types produced by
//!   `recover_stale`.

pub mod config;
pub mod error;
pub mod git;
pub mod manager;
pub mod measure;
pub mod provider;
pub mod recovery;

use std::fmt;

pub use error::WorktreeError;

/// A task's stable, collision-free identifier.
///
/// Task ids are modelled as a thin newtype over the `String` the IPC layer carries
/// (`nebula_proto` represents a task id as a `String`). The newtype keeps a task id from being
/// confused with any other string — a branch name, a ref, or a repository path — in the worktree
/// API and in [`WorktreeError`], while costing nothing at runtime.
///
/// A task id is used verbatim as the worktree directory name under `Worktrees_Dir` and, via the
/// configured `branch_name_format`, as the `Task_Branch` name. They are stable and collision-free
/// (settled decision), so no sanitization is applied here.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(String);

impl TaskId {
    /// Wrap an owned id string as a [`TaskId`].
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for TaskId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for TaskId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
