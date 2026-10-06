//! The `WorktreeManager` lifecycle API the executor (issue #32) drives.
//!
//! This module defines the [`WorktreeManager`] struct and its [`WorktreeManager::new`]
//! constructor. The lifecycle methods it will host — `create` / `finalize` / `remove_worktree` /
//! `recover_stale` — and the `Delete_Limit` measurement gate (which lives in the sibling `measure`
//! module) are filled by later tasks of the git-worktree-per-task feature (tasks 3.2, 5, 6, 7).
//! The executor issues no `git worktree` commands itself (Requirement 9.5); every git invocation
//! is internal to this module.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::approval::ApprovalStore;
use crate::worktree::TaskId;
use crate::worktree::config::{self, TASK_ID_PLACEHOLDER, WorktreeConfig};
use crate::worktree::error::WorktreeError;
use crate::worktree::git;
use crate::worktree::measure::{self, DeleteDecision};
use crate::worktree::recovery::{self, Disposition, RecoveryReport, StaleWorktree};

/// The per-task git worktree lifecycle manager the executor loop (issue #32) drives.
///
/// Holds the resolved `[worktree]` [`WorktreeConfig`], the configured `resources.retired_drive`
/// letter it rejects every path against (AGENTS.md hard rule 5), the per-repository creation locks
/// that serialize concurrent `create` calls on one repository (Requirement 3.5), the `task_id →`
/// [`Assignment`] registry of currently live tasks, and a reused handle to this crate's issue #28
/// [`ApprovalStore`] used to gate an over-limit cleanup deletion (Requirement 7.6). It defines no
/// second approval mechanism of its own.
pub struct WorktreeManager {
    /// The resolved `[worktree]` configuration (`worktrees_dir`, `branch_name_format`).
    config: WorktreeConfig,
    /// The configured `resources.retired_drive` string (for example `"C:"`); every resolved path
    /// is checked against it so no worktree is ever created on the retired drive.
    retired_drive: String,
    /// `task_id →` ([`Assignment`]) registry of currently live tasks. Behind a `tokio::sync::Mutex`
    /// because creation is `async` and the registry is mutated across `await` points.
    live: Arc<Mutex<HashMap<TaskId, Assignment>>>,
    /// Per-repository async creation locks, keyed by repository path, serializing concurrent
    /// `create` calls on the same repository (Requirement 3.5). The outer `Mutex` guards the map;
    /// each inner `Arc<Mutex<()>>` is the per-repository lock held across a single creation.
    repo_locks: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>,
    /// The reused issue #28 approval surface for the `Delete_Limit` gate (Requirement 7.6). An
    /// over-limit deletion emits an `ApprovalRequest` and removes nothing until an authorizing
    /// `Approval` is consumed from this store. Reused, not reinvented.
    approvals: Arc<ApprovalStore>,
}

/// A live task's worktree assignment: the repository it was created against, the task's
/// `Worktree_Root`, and its `Task_Branch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    /// The repository the worktree was created against — the `Main_Checkout` whose object store the
    /// worktree shares. Recorded at `create` time so `remove_worktree` knows where to run
    /// `git worktree remove` / `git worktree prune` (the registration is pruned from the owning
    /// repository, not from the worktree being removed).
    pub repo: PathBuf,
    /// The task's `Worktree_Root` — the directory `Worktrees_Dir\<task_id>` its worktree lives in.
    pub root: PathBuf,
    /// The task's `Task_Branch` name, formed from the configured `branch_name_format`.
    pub branch: String,
}

impl WorktreeManager {
    /// Build a `WorktreeManager` from the `[worktree]` config and the configured
    /// `resources.retired_drive`.
    ///
    /// `approvals` is the **reused** issue #28 [`ApprovalStore`] handle (shared with the permission
    /// engine, not a second mechanism); it gates over-limit cleanup deletions (Requirement 7.6).
    /// The live-task registry and the per-repository creation locks start empty.
    #[must_use]
    pub fn new(
        config: WorktreeConfig,
        retired_drive: String,
        approvals: Arc<ApprovalStore>,
    ) -> Self {
        Self {
            config,
            retired_drive,
            live: Arc::new(Mutex::new(HashMap::new())),
            repo_locks: Arc::new(Mutex::new(HashMap::new())),
            approvals,
        }
    }

    /// Create a git worktree and `Task_Branch` for `task_id` against `repo` and return the absolute
    /// `Worktree_Root`.
    ///
    /// With `start_point = None` the branch is based on `repo`'s current HEAD (its default-branch
    /// HEAD at the time of the call, Requirement 1.1); with `Some(reference)` it is based on that
    /// ref, which is rejected if it cannot be resolved (Requirement 1.2). The worktree is placed at
    /// `worktrees_dir/<task_id>` on the configured hot drive (Requirements 1.3, 8.3) and a new
    /// branch named by `branch_name_format` is checked out in it (Requirement 1.4). On success the
    /// `task_id → `[`Assignment`] mapping is recorded and the absolute root returned within the
    /// 30-second create budget (Requirement 1.6).
    ///
    /// Concurrent `create` calls for the **same** repository are serialized behind a per-repository
    /// async lock so two simultaneous requests cannot be assigned the same root or branch
    /// (Requirement 3.5).
    ///
    /// # Errors
    /// - [`WorktreeError::TaskAlreadyActive`] if `task_id` already has a live assignment
    ///   (Requirement 9.2).
    /// - [`WorktreeError::NotARepository`] if `repo` is not a git repository (Requirement 9.2).
    /// - [`WorktreeError::RetiredDrive`] if `worktrees_dir` or the computed target path is on the
    ///   retired drive — checked before any git call, nothing created (Requirements 1.9, 8.2).
    /// - [`WorktreeError::BranchExists`] if `refs/heads/<branch>` already exists — the branch is
    ///   left untouched and no worktree is created (Requirement 1.5).
    /// - [`WorktreeError::UnresolvableRef`] if a supplied `start_point` cannot be resolved —
    ///   nothing created (Requirement 1.2).
    /// - [`WorktreeError::CreateFailed`] if `git worktree add` itself fails. (Partial-failure
    ///   cleanup honouring the `Delete_Limit` is layered on in task 3.3.)
    pub async fn create(
        &self,
        task_id: &TaskId,
        repo: &Path,
        start_point: Option<&str>,
    ) -> Result<PathBuf, WorktreeError> {
        // Reject a task that is already live before touching anything (Requirement 9.2).
        if self.live.lock().await.contains_key(task_id) {
            return Err(WorktreeError::TaskAlreadyActive(task_id.clone()));
        }

        // Reject a target that is not a git repository before any further work (Requirement 9.2).
        if !git::is_git_repo(repo).await? {
            return Err(WorktreeError::NotARepository(repo.to_path_buf()));
        }

        // Serialize creation for this repository so two simultaneous requests cannot be assigned
        // the same root or branch (Requirement 3.5). Canonicalize the key so distinct spellings of
        // one repository share a lock; fall back to the given path if canonicalization fails.
        let repo_key = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
        let repo_lock = {
            let mut locks = self.repo_locks.lock().await;
            Arc::clone(
                locks
                    .entry(repo_key.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _repo_guard = repo_lock.lock().await;

        // Compute the target path and branch name (Requirements 1.3, 1.4, 8.3).
        let path = self.config.worktrees_dir.join(task_id.as_str());
        let branch = apply_branch_name_format(&self.config.branch_name_format, task_id);

        // Reject any path on the retired drive BEFORE any git call so nothing is created
        // (Requirements 1.9, 8.2). Check both the configured parent and the computed target.
        if config::on_retired_drive(&self.config.worktrees_dir, &self.retired_drive) {
            return Err(WorktreeError::RetiredDrive(
                self.config.worktrees_dir.clone(),
            ));
        }
        if config::on_retired_drive(&path, &self.retired_drive) {
            return Err(WorktreeError::RetiredDrive(path));
        }

        // Reject a pre-existing branch, leaving it untouched and creating no worktree (Req 1.5).
        if git::branch_exists(repo, &branch).await? {
            return Err(WorktreeError::BranchExists(branch));
        }

        // Resolve the start-point: the supplied ref (unresolvable → UnresolvableRef, nothing
        // created, Req 1.2) or the repository's current HEAD / default-branch HEAD (Req 1.1).
        let resolved_start = git::rev_parse_verify(repo, start_point.unwrap_or("HEAD")).await?;

        // Create the worktree and branch. On failure, clean up any partial directory honouring the
        // Delete_Limit before returning (Requirements 1.7, 1.8). No live assignment is recorded
        // until after a successful add, so any early-return error path below leaves `live` without
        // this `task_id` — a subsequent create for the same id is accepted (Requirement 9.6).
        if let Err(add_err) = git::worktree_add(repo, &branch, &path, &resolved_start).await {
            // Measure the partial directory and gate its removal against the Delete_Limit
            // (reuses the #28 approval contract via `prepare_delete`). The gate also absorbs a
            // measurement failure as its own error; on measurement failure we retain and surface
            // it (Requirement 7.5).
            let outcome = measure::prepare_delete(&path, &self.approvals, task_id.as_str()).await?;
            match outcome.decision {
                DeleteDecision::Proceed => {
                    // Within the limit: remove the partial directory and report CreateFailed. A
                    // not-yet-created directory (NotFound) is treated as already clean (Req 1.7).
                    if let Err(remove_err) = tokio::fs::remove_dir_all(&path).await
                        && remove_err.kind() != std::io::ErrorKind::NotFound
                    {
                        return Err(WorktreeError::CleanupFailed(format!(
                            "could not remove partial worktree {path:?} after a failed create: {remove_err}"
                        )));
                    }
                    return Err(WorktreeError::CreateFailed(add_err.to_string()));
                }
                DeleteDecision::AwaitingApproval { .. } => {
                    // Over the limit: remove nothing and retain the partial directory for manual
                    // cleanup (Requirement 1.8). The branch left by `git worktree add -b` is out of
                    // scope here; only the directory is reported.
                    return Err(WorktreeError::ManualCleanupRequired { path });
                }
            }
        }

        // Record the live assignment and return the absolute root (Requirement 1.6).
        let root = path.canonicalize().unwrap_or(path);
        self.live.lock().await.insert(
            task_id.clone(),
            Assignment {
                repo: repo_key,
                root: root.clone(),
                branch,
            },
        );
        Ok(root)
    }

    /// Finalize the task `task_id`, returning its retained `Task_Branch` name for issue #36's
    /// pull-request flow.
    ///
    /// Finalize is the "work is done" signal: it **retains** the `Task_Branch` (performs no
    /// `git push`, no PR, no merge — AGENTS.md hard rule 1) and returns its name so #36 can open a
    /// PR from it (Requirements 5.1, 9.3). It does **not** remove the worktree directory (that is
    /// [`remove_worktree`](Self::remove_worktree), Requirement 5.2) and does **not** delete the
    /// branch (Requirement 5.4). The task stays live after finalize, so a subsequent
    /// `remove_worktree` can still find its assignment.
    ///
    /// # Errors
    /// [`WorktreeError::UnknownTask`] if `task_id` has no live assignment (Requirement 9.2).
    pub async fn finalize(&self, task_id: &TaskId) -> Result<String, WorktreeError> {
        let live = self.live.lock().await;
        match live.get(task_id) {
            // Return the retained branch name; leave the assignment in `live` so the task remains
            // live until `remove_worktree` (Requirements 5.1, 5.2, 5.4, 9.3).
            Some(assignment) => Ok(assignment.branch.clone()),
            None => Err(WorktreeError::UnknownTask(task_id.clone())),
        }
    }

    /// Remove the task `task_id`'s worktree directory, keeping its `Task_Branch`.
    ///
    /// The completion cleanup step (called by the executor, issue #32, only after a PR is opened):
    ///
    /// 1. Looks up the live assignment; an absent one is [`WorktreeError::UnknownTask`]
    ///    (Requirement 9.2).
    /// 2. Measures the directory and applies the `Delete_Limit` gate (reusing the #28 approval
    ///    contract). Over the limit with no authorizing approval → [`WorktreeError::DeleteLimit`],
    ///    removing nothing (Requirement 5.5).
    /// 3. Runs `git worktree remove <path>`, which **refuses a dirty tree**; a dirty-tree refusal
    ///    maps to [`WorktreeError::DirtyWorktree`], deleting nothing (Requirement 5.6). Any other
    ///    non-zero exit maps to [`WorktreeError::CleanupFailed`], leaving things intact
    ///    (Requirement 5.7).
    /// 4. On success runs `git worktree prune` (Requirement 5.3); a prune failure is
    ///    [`WorktreeError::CleanupFailed`], leaving the branch and any registration intact
    ///    (Requirement 5.7).
    /// 5. On full success drops the task from the live registry and **keeps the branch**
    ///    (Requirement 5.4).
    ///
    /// # Errors
    /// - [`WorktreeError::UnknownTask`] if `task_id` has no live assignment.
    /// - [`WorktreeError::DeleteLimit`] if the directory meets or exceeds the delete-limit with no
    ///   authorizing approval.
    /// - [`WorktreeError::DirtyWorktree`] if the worktree has uncommitted work.
    /// - [`WorktreeError::CleanupFailed`] if `git worktree remove` (for a non-dirty reason) or
    ///   `git worktree prune` fails.
    /// - [`WorktreeError::Git`] if a git invocation cannot be spawned or times out.
    pub async fn remove_worktree(&self, task_id: &TaskId) -> Result<(), WorktreeError> {
        // Snapshot the assignment, releasing the registry lock before the git calls so a slow
        // removal does not block other tasks' lookups.
        let assignment = {
            let live = self.live.lock().await;
            match live.get(task_id) {
                Some(assignment) => assignment.clone(),
                None => return Err(WorktreeError::UnknownTask(task_id.clone())),
            }
        };

        // Delete_Limit gate: over the limit with no approval → remove nothing (Requirement 5.5).
        let outcome =
            measure::prepare_delete(&assignment.root, &self.approvals, task_id.as_str()).await?;
        if let Some(limit_err) = outcome.delete_limit_error() {
            return Err(limit_err);
        }

        // `git worktree remove` refuses a dirty tree. Run it from the owning repository so the
        // registration is removed from the right common dir (Requirements 5.2, 5.6).
        let removed = git::worktree_remove(&assignment.repo, &assignment.root).await?;
        if !removed.success {
            // git's dirty-tree refusal names the modified/untracked files; map it to DirtyWorktree
            // and delete nothing (Requirement 5.6). Any other non-zero exit is CleanupFailed
            // leaving things intact (Requirement 5.7).
            if is_dirty_tree_refusal(&removed.stderr) {
                return Err(WorktreeError::DirtyWorktree(assignment.root.clone()));
            }
            return Err(WorktreeError::CleanupFailed(format!(
                "git worktree remove {:?} failed: {}",
                assignment.root,
                removed.stderr.trim()
            )));
        }

        // Clear the removed registration (Requirement 5.3). A prune failure leaves the branch and
        // any registration intact and surfaces CleanupFailed (Requirement 5.7).
        git::worktree_prune(&assignment.repo)
            .await
            .map_err(|e| WorktreeError::CleanupFailed(e.to_string()))?;

        // Full success: drop the live assignment and keep the branch (Requirement 5.4).
        self.live.lock().await.remove(task_id);
        Ok(())
    }

    /// Detect `Stale_Worktree`s left under `Worktrees_Dir` by a crash and dispose of each safely.
    ///
    /// Run at daemon start. A `Stale_Worktree` is a worktree still registered in git, located
    /// **under `Worktrees_Dir`**, with no live `Task` owning it (Requirement 6.1). Recovery never
    /// deletes, prunes, or modifies any worktree **outside** `Worktrees_Dir` (Requirement 6.2).
    ///
    /// Each candidate is disposed of as follows:
    /// - dirty or carrying unmerged commits → [`Disposition::RetainedForReview`], never deleted
    ///   (Requirement 6.5);
    /// - indeterminate work-status (a git query errored) → [`Disposition::RetainedForReview`],
    ///   never deleted (Requirement 6.8);
    /// - clean and within the `Delete_Limit` → `git worktree remove` + prune,
    ///   [`Disposition::Removed`] (Requirement 6.6);
    /// - clean but over the `Delete_Limit` → a #28 `ApprovalRequest` is emitted and the worktree is
    ///   retained as [`Disposition::AwaitingApproval`] until an authorizing approval is granted
    ///   (Requirement 6.7).
    ///
    /// Each detected stale worktree is reported with its root, branch, and `Uncommitted_Work` flag
    /// (Requirements 6.3, 9.4).
    ///
    /// # Repo discovery (design-signature note)
    ///
    /// The design gives `recover_stale(&self)` no repository argument, but
    /// `git worktree list --porcelain` must run from *some* repository. The manager is not bound to
    /// a single repository, so recovery enumerates the immediate subdirectories of `Worktrees_Dir`
    /// and, for each one that is itself a registered worktree, runs
    /// `git worktree list --porcelain` from inside it. A worktree shares its owning repository's
    /// registry, so that one listing enumerates every worktree of that repository; recovery keeps
    /// only the listed entries that lie under `Worktrees_Dir` (Requirement 6.2) and skips the main
    /// worktree (the repository itself, which is not under `Worktrees_Dir`). Candidates already
    /// seen (reachable from more than one sibling's listing) are de-duplicated by root. If
    /// `Worktrees_Dir` does not exist or cannot be read, there is nothing to recover and an empty
    /// report is returned.
    ///
    /// This method never returns an error: an indeterminate candidate is retained and reported
    /// rather than failing the whole pass (Requirement 6.8).
    pub async fn recover_stale(&self) -> RecoveryReport {
        let mut report = RecoveryReport::default();

        // Enumerate immediate subdirectories of Worktrees_Dir. If it is absent or unreadable there
        // is nothing to recover (Requirement 6.1); return an empty report.
        let worktrees_dir = &self.config.worktrees_dir;
        let Ok(mut dir) = tokio::fs::read_dir(worktrees_dir).await else {
            return report;
        };

        // Collect candidate subdirectories first; each may host a worktree whose listing reveals
        // its siblings under Worktrees_Dir. A read error mid-enumeration stops the walk; recovery
        // proceeds with whatever was collected.
        let mut subdirs: Vec<PathBuf> = Vec::new();
        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();
            if let Ok(meta) = tokio::fs::metadata(&path).await
                && meta.is_dir()
            {
                subdirs.push(path);
            }
        }

        let live = self.live.lock().await;
        // Roots already turned into a StaleWorktree, to de-duplicate across sibling listings.
        let mut seen: Vec<PathBuf> = Vec::new();

        for subdir in &subdirs {
            // List the worktrees of the repository this subdirectory belongs to (if it is one).
            let listing = match git::worktree_list_porcelain(subdir).await {
                Ok(out) => out.stdout,
                // Not a worktree, or git failed here: skip this subdirectory.
                Err(_) => continue,
            };

            let entries = parse_worktree_list(&listing);

            // The owning repository is the listing's main worktree — the one entry that is NOT
            // under Worktrees_Dir. `git worktree remove`/`prune` must run from there, never from a
            // worktree being deleted: on Windows a directory that is a process's CWD is locked, so
            // removing the worktree while git's `current_dir` is that same directory fails
            // (Requirement 6.6). Fall back to the subdir only if no main entry is found (every
            // listed worktree being under Worktrees_Dir is not expected for a real repository).
            let owning_repo = entries
                .iter()
                .map(|(root, _)| root)
                .find(|root| !is_under(root, worktrees_dir))
                .cloned()
                .unwrap_or_else(|| subdir.clone());

            for (root, branch) in entries {
                // Requirement 6.2: only ever touch entries under Worktrees_Dir.
                if !is_under(&root, worktrees_dir) {
                    continue;
                }
                // De-duplicate: a root may appear in several siblings' listings.
                if seen.iter().any(|r| r == &root) {
                    continue;
                }
                // A worktree owned by a live task is not stale (Requirement 6.1).
                if live
                    .values()
                    .any(|assignment| paths_equal(&assignment.root, &root))
                {
                    continue;
                }
                seen.push(root.clone());

                // A branch is required to assess uncommitted work; a detached/bare entry has none.
                let Some(branch) = branch else {
                    // No branch to evaluate: indeterminate → retain and report (Requirement 6.8).
                    report.stale.push(StaleWorktree {
                        root,
                        branch: String::new(),
                        has_uncommitted_work: false,
                        disposition: Disposition::RetainedForReview,
                    });
                    continue;
                };

                let stale = self.dispose_stale(&owning_repo, root, branch).await;
                report.stale.push(stale);
            }
        }

        report
    }

    /// Decide and apply the [`Disposition`] of one stale worktree at `root` on `branch`.
    ///
    /// `owning_repo` is the main worktree of the repository that registered `root` (the listing
    /// entry not under `Worktrees_Dir`); `git worktree remove`/`prune` run from there so git's
    /// `current_dir` is never the directory being deleted. On Windows a directory that is a
    /// process's working directory is locked, so running the removal from inside `root` fails and a
    /// clean worktree within the limit would never be removed (Requirement 6.6).
    ///
    /// Honours Requirements 6.5 (dirty/unmerged → retain), 6.8 (indeterminate → retain), 6.6 (clean
    /// & within limit → remove + prune), and 6.7 (clean & over limit → await approval). Never
    /// deletes a worktree carrying work or whose status could not be determined.
    async fn dispose_stale(
        &self,
        owning_repo: &Path,
        root: PathBuf,
        branch: String,
    ) -> StaleWorktree {
        // Uncommitted_Work: an error is indeterminate → retain and report (Requirement 6.8).
        let Ok(has_uncommitted_work) = recovery::has_uncommitted_work(&root, &branch).await else {
            return StaleWorktree {
                root,
                branch,
                has_uncommitted_work: false,
                disposition: Disposition::RetainedForReview,
            };
        };

        // Dirty or unmerged → retain for review, never deleted (Requirement 6.5).
        if has_uncommitted_work {
            return StaleWorktree {
                root,
                branch,
                has_uncommitted_work: true,
                disposition: Disposition::RetainedForReview,
            };
        }

        // Clean: gate removal against the Delete_Limit (Requirements 6.6, 6.7). A measurement
        // failure is indeterminate → retain (Requirement 6.8).
        let Ok(outcome) = measure::prepare_delete(&root, &self.approvals, &branch).await else {
            return StaleWorktree {
                root,
                branch,
                has_uncommitted_work: false,
                disposition: Disposition::RetainedForReview,
            };
        };

        match outcome.decision {
            DeleteDecision::AwaitingApproval { bytes, files } => {
                // Over the limit: a request was emitted; retain until granted (Requirement 6.7).
                StaleWorktree {
                    root,
                    branch,
                    has_uncommitted_work: false,
                    disposition: Disposition::AwaitingApproval { bytes, files },
                }
            }
            DeleteDecision::Proceed => {
                // Clean and within the limit: remove + prune → Removed (Requirement 6.6). Run git
                // from `owning_repo` (the main worktree, never `root` itself) so git's working
                // directory is not the directory being deleted — on Windows that directory would be
                // locked and the removal would fail. A failure to remove/prune falls back to
                // retaining and reporting rather than claiming a removal that did not happen
                // (Requirement 6.8).
                let removed = git::worktree_remove(owning_repo, &root).await;
                let disposition = match removed {
                    Ok(out) if out.success => match git::worktree_prune(owning_repo).await {
                        Ok(_) => Disposition::Removed,
                        Err(_) => Disposition::RetainedForReview,
                    },
                    _ => Disposition::RetainedForReview,
                };
                StaleWorktree {
                    root,
                    branch,
                    has_uncommitted_work: false,
                    disposition,
                }
            }
        }
    }
}

/// Whether `candidate` is `base` itself or lies beneath it, comparing on canonicalized paths when
/// possible so distinct spellings of one directory match.
///
/// Used by [`WorktreeManager::recover_stale`] to keep only worktrees under `Worktrees_Dir`
/// (Requirement 6.2).
fn is_under(candidate: &Path, base: &Path) -> bool {
    // Prefer canonicalized comparison so distinct spellings (e.g. `\\?\` prefixes, `.` segments) of
    // one directory match. Both the listed worktree root and the configured Worktrees_Dir normally
    // exist on disk during recovery, so canonicalization succeeds.
    match (candidate.canonicalize(), base.canonicalize()) {
        (Ok(candidate), Ok(base)) => candidate.starts_with(&base),
        // If either path cannot be canonicalized, fall back to a strict lexical comparison. This is
        // conservative: it only ever keeps a path that is lexically beneath `base`, so recovery
        // never touches anything outside Worktrees_Dir (Requirement 6.2).
        _ => candidate.starts_with(base),
    }
}

/// Whether two paths name the same location, comparing canonicalized forms when possible.
fn paths_equal(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Whether a `git worktree remove` stderr is git's dirty-tree refusal rather than another failure.
///
/// git refuses to remove a worktree with local changes with a message naming "modified or
/// untracked files" and suggesting `--force`; recovery and completion both map that exact refusal
/// to [`WorktreeError::DirtyWorktree`] (Requirement 5.6) while treating any other non-zero exit as
/// a generic [`WorktreeError::CleanupFailed`].
fn is_dirty_tree_refusal(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    // git's refusal: "'<path>' contains modified or untracked files, use --force to delete it".
    lower.contains("contains modified or untracked files")
        || (lower.contains("use --force")
            && (lower.contains("modified") || lower.contains("untracked")))
}

/// Parse `git worktree list --porcelain` output into `(root, branch)` pairs.
///
/// Records are separated by blank lines; within a record the lines of interest are
/// `worktree <path>`, `branch refs/heads/<name>`, and the `detached` / `bare` markers. A `bare`
/// record (the bare main repository) is skipped. A record with no `branch` line (detached HEAD)
/// yields `None` for the branch so the caller can treat it as indeterminate.
fn parse_worktree_list(porcelain: &str) -> Vec<(PathBuf, Option<String>)> {
    /// One in-progress `git worktree list --porcelain` record being accumulated line by line.
    #[derive(Default)]
    struct WorktreeRecord {
        root: Option<PathBuf>,
        branch: Option<String>,
        bare: bool,
    }

    let mut out = Vec::new();
    let mut record = WorktreeRecord::default();

    // Flush the record being accumulated into the output (unless it is the bare main repo).
    let flush = |record: &mut WorktreeRecord, out: &mut Vec<(PathBuf, Option<String>)>| {
        if let Some(root) = record.root.take()
            && !record.bare
        {
            out.push((root, record.branch.take()));
        }
        record.branch = None;
        record.bare = false;
    };

    for line in porcelain.lines() {
        if line.is_empty() {
            flush(&mut record, &mut out);
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            // A new `worktree` line starts a new record; flush any in-progress one defensively.
            flush(&mut record, &mut out);
            record.root = Some(PathBuf::from(path));
        } else if let Some(refname) = line.strip_prefix("branch ") {
            record.branch = Some(
                refname
                    .strip_prefix("refs/heads/")
                    .unwrap_or(refname)
                    .to_owned(),
            );
        } else if line == "bare" {
            record.bare = true;
        }
        // `HEAD <oid>` and `detached` carry no branch; left as the default `None`.
    }
    // Flush the final record (porcelain output may not end with a blank line).
    flush(&mut record, &mut out);

    out
}

/// Build a `Task_Branch` name by substituting the `{task_id}` placeholder in `format` with
/// `task_id` (default format `nebula/{task_id}` → `nebula/<task-id>`, Requirement 1.4).
///
/// [`WorktreeConfig::validate`](crate::worktree::config::WorktreeConfig::validate) guarantees a
/// configured `branch_name_format` contains [`TASK_ID_PLACEHOLDER`]; a format without it would
/// yield the same branch name for every task and is rejected at config-validation time.
fn apply_branch_name_format(format: &str, task_id: &TaskId) -> String {
    format.replace(TASK_ID_PLACEHOLDER, task_id.as_str())
}
