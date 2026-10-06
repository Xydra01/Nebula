//! The single Tool-level `WorktreeError` returned by every `WorktreeManager` operation.
//!
//! This is the one error value the executor matches on; the manager issues no git commands on the
//! executor's behalf that leak a raw git error (Requirement 9.5). Each variant carries a
//! descriptive message so a failure is actionable without inspecting a nested source.
//!
//! The enum is re-exported from the module root as [`crate::worktree::error::WorktreeError`]; the
//! sibling modules (`config`, `git`, `manager`, `provider`, `recovery`) compile against this one
//! complete error type.

use std::path::PathBuf;

use crate::worktree::TaskId;

/// The single Tool-level error returned by every [`WorktreeManager`] operation (Requirement 9.5).
///
/// The executor matches on this one enum and never sees a raw git error. Each variant carries a
/// descriptive message so a failure is actionable without inspecting a nested source.
///
/// [`WorktreeManager`]: crate::worktree::manager::WorktreeManager
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorktreeError {
    /// A `create` was requested for a task that already has a live worktree assignment
    /// (Requirement 9.2). Nothing is created.
    #[error("task {0:?} already has an active worktree")]
    TaskAlreadyActive(TaskId),
    /// An operation (`finalize`, `remove_worktree`) named a task with no live assignment
    /// (Requirement 9.2).
    #[error("unknown task {0:?}")]
    UnknownTask(TaskId),
    /// The target `repo` is not a git repository (Requirement 9.2). Nothing is created.
    #[error("{0:?} is not a git repository")]
    NotARepository(PathBuf),
    /// A specified start-point ref could not be resolved in the target repository (Requirement 1.2).
    /// Nothing is created.
    #[error("start-point ref {0:?} could not be resolved")]
    UnresolvableRef(String),
    /// A branch with the resolved `Task_Branch` name already exists; the branch is left untouched
    /// and no worktree is created (Requirement 1.5).
    #[error("branch {0:?} is already in use")]
    BranchExists(String),
    /// A resolved worktree path, or the configured `worktrees_dir`, lies on the retired drive and
    /// must never be used (AGENTS.md hard rule 5; Requirements 1.9, 8.2). Nothing is created.
    #[error("path {0:?} resolves to the retired drive")]
    RetiredDrive(PathBuf),
    /// Worktree creation failed; any partial directory within the delete-limit was cleaned up, so
    /// no live assignment remains (Requirement 1.7).
    #[error("worktree creation failed: {0}")]
    CreateFailed(String),
    /// A worktree still contains uncommitted work and was therefore not removed (Requirement 5.6).
    #[error("worktree {0:?} has uncommitted work and was not removed")]
    DirtyWorktree(PathBuf),
    /// A deletion met or exceeded the delete-limit and awaits owner approval (Requirements 5.5,
    /// 7.3). Nothing is removed until an authorizing approval is consumed.
    #[error(
        "deletion of {bytes} bytes / {files} files exceeds the delete limit and awaits approval"
    )]
    DeleteLimit {
        /// Total measured bytes of the pending deletion.
        bytes: u64,
        /// Total measured file count of the pending deletion.
        files: u64,
    },
    /// Partial-failure cleanup would exceed the delete-limit, so the directory was retained for
    /// manual cleanup (Requirement 1.8).
    #[error("cleanup of {path:?} would exceed the delete limit; manual cleanup required")]
    ManualCleanupRequired {
        /// The retained directory the owner must clean up by hand.
        path: PathBuf,
    },
    /// Worktree-directory removal or `git worktree prune` did not complete; the branch and any
    /// unremoved registration are left intact (Requirement 5.7).
    #[error("cleanup did not complete: {0}")]
    CleanupFailed(String),
    /// A git invocation failed, timed out, or could not be spawned (Requirement 9.5).
    #[error("git invocation failed: {0}")]
    Git(String),
    /// The `[worktree]` configuration is invalid: a `branch_name_format` missing the `{task_id}`
    /// placeholder, a `worktrees_dir` on the retired drive, or another self-inconsistent setting.
    /// Carries a message naming the problem. Raised by
    /// [`WorktreeConfig::validate`](crate::worktree::config::WorktreeConfig::validate).
    #[error("invalid worktree config: {0}")]
    InvalidConfig(String),
}
