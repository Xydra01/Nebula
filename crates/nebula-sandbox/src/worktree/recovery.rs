//! Crash-recovery types for [`WorktreeManager::recover_stale`].
//!
//! At daemon start the manager enumerates the git worktree registry, keeps only entries under the
//! configured `Worktrees_Dir`, and treats each one with no live `Task` owner as a `Stale_Worktree`
//! (Requirement 6.1). This module defines the value types that pass describes its result with —
//! [`RecoveryReport`], [`StaleWorktree`], and [`Disposition`] — and the [`has_uncommitted_work`]
//! predicate the pass uses to decide a worktree's fate.
//!
//! A worktree carrying `Uncommitted_Work` is **retained and reported, never deleted**
//! (Requirement 6.5), and recovery never touches a worktree outside `Worktrees_Dir`
//! (Requirement 6.2). The pass itself (`recover_stale`) lives in
//! [`manager`](crate::worktree::manager); this module supplies only the report shape and the
//! work-detection predicate.
//!
//! [`WorktreeManager::recover_stale`]: crate::worktree::manager::WorktreeManager::recover_stale

use std::path::{Path, PathBuf};

use crate::worktree::error::WorktreeError;
use crate::worktree::git;

/// The result of a [`recover_stale`] pass (Requirements 6.3, 9.4).
///
/// It lists every `Stale_Worktree` detected under `Worktrees_Dir`, each tagged with its
/// [`Disposition`]. An empty [`stale`](Self::stale) list means recovery found nothing to report.
/// The daemon logs this report at start so leftover work is surfaced rather than silently
/// discarded.
///
/// [`recover_stale`]: crate::worktree::manager::WorktreeManager::recover_stale
#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    /// Stale worktrees detected under `Worktrees_Dir`, each with its [`Disposition`].
    pub stale: Vec<StaleWorktree>,
}

/// One detected `Stale_Worktree` and what recovery did with it (Requirement 6.3).
///
/// A `Stale_Worktree` is a worktree still registered in git after the daemon stopped or crashed,
/// located under `Worktrees_Dir`, with no live `Task` owning it (Requirement 6.1). Every detected
/// one is reported with its root, branch, and uncommitted-work flag so the owner can review it.
#[derive(Clone, Debug)]
pub struct StaleWorktree {
    /// The absolute `Worktree_Root` of the stale worktree.
    pub root: PathBuf,
    /// The `Task_Branch` checked out in the stale worktree.
    pub branch: String,
    /// `true` when the worktree is dirty or carries unmerged commits (Requirement 6.4).
    ///
    /// When `true`, the worktree is retained for review and never deleted (Requirement 6.5).
    pub has_uncommitted_work: bool,
    /// What recovery did with this worktree.
    pub disposition: Disposition,
}

/// What [`recover_stale`] did with a stale worktree.
///
/// [`recover_stale`]: crate::worktree::manager::WorktreeManager::recover_stale
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Clean and within the `Delete_Limit`: the worktree was removed and pruned (Requirement 6.6).
    Removed,
    /// Dirty, carrying unmerged commits, or indeterminate: kept for owner review and not deleted
    /// (Requirements 6.5, 6.8).
    RetainedForReview,
    /// Clean but over the `Delete_Limit`: retained pending an authorizing issue #28 approval
    /// (Requirement 6.7). Carries the measured byte and file totals of the pending deletion.
    AwaitingApproval {
        /// Total measured bytes of the pending deletion.
        bytes: u64,
        /// Total measured file count of the pending deletion.
        files: u64,
    },
}

/// Determine whether a worktree contains `Uncommitted_Work` (Requirement 6.4).
///
/// A worktree has uncommitted work when **either** condition holds:
///
/// - its working tree is dirty — `git status --porcelain` for `worktree_root` is non-empty
///   (tracked modifications, staged changes, or non-ignored untracked files); **or**
/// - its `Task_Branch` carries commits reachable from no other ref — the unmerged commit count for
///   `branch` is greater than zero.
///
/// Either condition means the worktree is carrying work, so a caller must retain it rather than
/// delete it (Requirement 6.5). An error from either git query propagates to the caller, which
/// treats an indeterminate work-status as a reason to retain and report rather than delete
/// (Requirement 6.8).
///
/// Both git queries run with `current_dir` set to `worktree_root`; the worktree shares the
/// repository object store with the `Main_Checkout`, so the `rev-list` behind
/// [`git::unmerged_commit_count`] sees every ref.
pub async fn has_uncommitted_work(
    worktree_root: &Path,
    branch: &str,
) -> Result<bool, WorktreeError> {
    if !git::status_porcelain(worktree_root).await?.is_empty() {
        return Ok(true);
    }
    Ok(git::unmerged_commit_count(worktree_root, branch).await? > 0)
}
