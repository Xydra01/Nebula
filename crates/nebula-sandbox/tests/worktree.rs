#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

//! Integration tests for the `WorktreeManager` lifecycle over REAL temporary git repositories.
//!
//! These tests drive the public `nebula_sandbox::worktree` API end to end — `create`, `finalize`,
//! `remove_worktree`, and `recover_stale` — against repositories built on `tempfile` tempdirs
//! exactly as the in-crate `git.rs` tests do (`git init`, a local committer identity, a seed
//! commit). Every path lives under a tempdir; no real path and never `C:` is touched.
//!
//! Covered tasks of the git-worktree-per-task spec:
//! - 3.4 — create happy path, specified tag, unresolvable ref, branch collision, retired drive.
//! - 5.3 — completion lifecycle: dirty-tree refusal, finalize retains the branch, clean removal.
//! - 7.5 — crash recovery: a stale dirty worktree is retained, a clean one removed, and a worktree
//!   outside `Worktrees_Dir` is never listed or touched.
//! - 7.3 — Property 5: a worktree with uncommitted work is never deleted.
//! - 7.4 — Property 9: crash recovery never touches worktrees outside `Worktrees_Dir`.

use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::Arc;

use tempfile::TempDir;

use nebula_sandbox::approval::ApprovalStore;
use nebula_sandbox::worktree::TaskId;
use nebula_sandbox::worktree::config::WorktreeConfig;
use nebula_sandbox::worktree::error::WorktreeError;
use nebula_sandbox::worktree::manager::WorktreeManager;
use nebula_sandbox::worktree::recovery::Disposition;

/// Run `git <args>` in `dir`, asserting success. Mirrors the arrangement helper the in-crate
/// `git.rs` tests use to build a real repository.
fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

/// Run `git <args>` in `dir` and capture trimmed stdout, asserting success. Used for queries such
/// as `rev-parse` where the resolved value matters.
fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed in {dir:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Whether a local branch resolves in `repo` (its `refs/heads/<branch>` is a real commit).
fn branch_resolves(repo: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(repo)
        .status()
        .unwrap()
        .success()
}

/// Create a fresh initialized repo with a local committer identity and a single seed commit, so a
/// HEAD exists for ref resolution — the same pattern as the in-crate `git.rs` `seed_repo`.
fn seed_repo() -> TempDir {
    let d = TempDir::new().unwrap();
    git(d.path(), &["init"]);
    git(d.path(), &["config", "user.email", "t@example.com"]);
    git(d.path(), &["config", "user.name", "t"]);
    // Keep the working tree byte-identical to committed blobs so a freshly created worktree reports
    // clean. On Windows the machine's `core.autocrlf=true` would otherwise rewrite LF->CRLF on
    // checkout and make `git status --porcelain` non-empty for an untouched file (a false "dirty").
    // Set locally on this throwaway temp repo only — the machine's git config is left untouched.
    git(d.path(), &["config", "core.autocrlf", "false"]);
    std::fs::write(d.path().join("seed.txt"), "seed\n").unwrap();
    git(d.path(), &["add", "seed.txt"]);
    git(d.path(), &["commit", "-m", "seed"]);
    d
}

/// Build a `WorktreeManager` for `worktrees_dir` rejecting `retired_drive`, with the default branch
/// format and an empty (issue #28) approval store.
fn manager(worktrees_dir: &Path, retired_drive: &str) -> WorktreeManager {
    let config = WorktreeConfig {
        worktrees_dir: worktrees_dir.to_path_buf(),
        branch_name_format: "nebula/{task_id}".to_owned(),
    };
    WorktreeManager::new(
        config,
        retired_drive.to_owned(),
        Arc::new(ApprovalStore::new()),
    )
}

/// Extract the `"<letter>:"` drive prefix of `path` (for example `"F:"`), used to point a
/// manager's `retired_drive` at the drive its tempdir lives on for the rejection test. Never
/// returns `C:` literally — it reflects wherever the OS placed the tempdir (here, `F:`).
fn drive_of(path: &Path) -> String {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(byte) | Prefix::VerbatimDisk(byte) => {
                format!("{}:", byte as char)
            }
            _ => panic!("tempdir path {path:?} has no drive-letter prefix"),
        },
        _ => panic!("tempdir path {path:?} has no prefix component"),
    }
}

// ===========================================================================================
// Task 3.4 — create: happy path, specified tag, unresolvable ref, branch collision, retired drive
// ===========================================================================================

/// `create` returns an absolute root under `worktrees_dir` with the task's branch checked out
/// (Req 9.1). The returned root is absolute, lives under the configured parent, and the worktree
/// has `nebula/<task-id>` checked out.
#[tokio::test]
async fn create_returns_absolute_root_with_branch_checked_out() {
    let repo = seed_repo();
    let wt = TempDir::new().unwrap();
    let mgr = manager(wt.path(), "Q:");

    let task = TaskId::new("task-happy");
    let root = mgr.create(&task, repo.path(), None).await.unwrap();

    assert!(
        root.is_absolute(),
        "returned root {root:?} must be absolute"
    );
    // The root is `worktrees_dir/<task-id>` (compare canonicalized to absorb `\\?\` prefixes).
    let expected = wt.path().join("task-happy").canonicalize().unwrap();
    assert_eq!(root.canonicalize().unwrap(), expected);
    assert!(root.is_dir(), "the worktree directory should exist");

    // The branch `nebula/task-happy` is checked out in the worktree.
    let head = git_stdout(&root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head, "nebula/task-happy", "the task branch is checked out");
    assert!(
        branch_resolves(repo.path(), "nebula/task-happy"),
        "the task branch resolves in the owning repo"
    );
}

/// `create` off a real tag succeeds, basing the worktree on the tagged commit (Req 1.2, happy
/// side). A tag `v1` is created in the seed repo and used as the start point.
#[tokio::test]
async fn create_off_a_real_tag_succeeds() {
    let repo = seed_repo();
    git(repo.path(), &["tag", "v1"]);
    let tagged = git_stdout(repo.path(), &["rev-parse", "v1"]);

    let wt = TempDir::new().unwrap();
    let mgr = manager(wt.path(), "Q:");

    let task = TaskId::new("task-tag");
    let root = mgr.create(&task, repo.path(), Some("v1")).await.unwrap();

    // The worktree's HEAD commit equals the tagged commit.
    let head_oid = git_stdout(&root, &["rev-parse", "HEAD"]);
    assert_eq!(
        head_oid, tagged,
        "the worktree is based on the tagged commit"
    );
}

/// A bogus start-point ref maps to [`WorktreeError::UnresolvableRef`] and creates nothing
/// (Req 1.2). No worktree directory is left behind and no live assignment remains.
#[tokio::test]
async fn create_with_a_bogus_ref_is_unresolvable_and_creates_nothing() {
    let repo = seed_repo();
    let wt = TempDir::new().unwrap();
    let mgr = manager(wt.path(), "Q:");

    let task = TaskId::new("task-bogus");
    let err = mgr
        .create(&task, repo.path(), Some("does-not-exist"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, WorktreeError::UnresolvableRef(ref r) if r == "does-not-exist"),
        "expected UnresolvableRef, got {err:?}"
    );
    // Nothing created: no worktree directory for the task.
    assert!(
        !wt.path().join("task-bogus").exists(),
        "an unresolvable ref must create no worktree directory"
    );
    // No live assignment: a subsequent valid create for the same id is accepted.
    mgr.create(&task, repo.path(), None).await.unwrap();
}

/// A pre-existing branch collision maps to [`WorktreeError::BranchExists`], leaves the branch
/// untouched, and creates no worktree directory (Req 1.5).
#[tokio::test]
async fn create_with_a_preexisting_branch_is_rejected_leaving_it_untouched() {
    let repo = seed_repo();
    // Pre-create the branch the task would want: `nebula/task-collide`.
    git(repo.path(), &["branch", "nebula/task-collide"]);
    let before = git_stdout(repo.path(), &["rev-parse", "nebula/task-collide"]);

    let wt = TempDir::new().unwrap();
    let mgr = manager(wt.path(), "Q:");

    let task = TaskId::new("task-collide");
    let err = mgr.create(&task, repo.path(), None).await.unwrap_err();
    assert!(
        matches!(err, WorktreeError::BranchExists(ref b) if b == "nebula/task-collide"),
        "expected BranchExists, got {err:?}"
    );
    // The branch is unchanged and no worktree directory was created.
    let after = git_stdout(repo.path(), &["rev-parse", "nebula/task-collide"]);
    assert_eq!(before, after, "the existing branch must be left untouched");
    assert!(
        !wt.path().join("task-collide").exists(),
        "a branch collision must create no worktree directory"
    );
}

/// A `worktrees_dir` on the retired drive maps to [`WorktreeError::RetiredDrive`] and creates
/// nothing (Req 1.9, 8.2). The retired drive is set to the drive the tempdir actually lives on
/// (extracted from its prefix — on this machine `F:`), never literally `C:`.
#[tokio::test]
async fn create_on_the_retired_drive_is_rejected_creating_nothing() {
    let repo = seed_repo();
    let wt = TempDir::new().unwrap();
    // Treat the tempdir's own drive as retired, so the configured worktrees_dir is on it.
    let retired = drive_of(wt.path());
    let mgr = manager(wt.path(), &retired);

    let task = TaskId::new("task-retired");
    let err = mgr.create(&task, repo.path(), None).await.unwrap_err();
    assert!(
        matches!(err, WorktreeError::RetiredDrive(_)),
        "expected RetiredDrive, got {err:?}"
    );
    assert!(
        !wt.path().join("task-retired").exists(),
        "a retired-drive rejection must create no worktree directory"
    );
}

// ===========================================================================================
// Task 5.3 — completion lifecycle: dirty-tree refusal, finalize retains branch, clean removal
// ===========================================================================================

/// A dirty worktree cannot be removed: `remove_worktree` maps git's refusal to
/// [`WorktreeError::DirtyWorktree`] and the directory is retained (Req 5.6).
#[tokio::test]
async fn remove_worktree_refuses_a_dirty_tree_and_retains_it() {
    let repo = seed_repo();
    let wt = TempDir::new().unwrap();
    let mgr = manager(wt.path(), "Q:");

    let task = TaskId::new("task-dirty");
    let root = mgr.create(&task, repo.path(), None).await.unwrap();

    // Make the worktree dirty with an untracked file so git refuses to remove it.
    std::fs::write(root.join("scratch.txt"), "uncommitted\n").unwrap();

    let err = mgr.remove_worktree(&task).await.unwrap_err();
    assert!(
        matches!(err, WorktreeError::DirtyWorktree(_)),
        "expected DirtyWorktree, got {err:?}"
    );
    assert!(
        root.exists(),
        "a dirty worktree must be retained, not deleted"
    );
}

/// `finalize` returns the retained `Task_Branch` name and the branch still resolves afterwards;
/// then `remove_worktree` on the now-clean worktree removes only that directory, prunes, and leaves
/// the branch resolvable (Req 5.1, 5.3, 5.4).
#[tokio::test]
async fn finalize_then_remove_keeps_the_branch_and_removes_only_the_directory() {
    let repo = seed_repo();
    let wt = TempDir::new().unwrap();
    let mgr = manager(wt.path(), "Q:");

    let task = TaskId::new("task-clean");
    let root = mgr.create(&task, repo.path(), None).await.unwrap();

    // finalize returns the retained branch name, and the branch still resolves.
    let branch = mgr.finalize(&task).await.unwrap();
    assert_eq!(
        branch, "nebula/task-clean",
        "finalize returns the task branch"
    );
    assert!(
        branch_resolves(repo.path(), &branch),
        "finalize retains the branch (it still resolves)"
    );

    // The worktree is clean (no changes), so remove_worktree removes just its directory and prunes.
    mgr.remove_worktree(&task).await.unwrap();
    assert!(
        !root.exists(),
        "remove_worktree deletes the worktree directory"
    );
    // The branch is kept and still resolves after removal.
    assert!(
        branch_resolves(repo.path(), &branch),
        "remove_worktree keeps the branch resolvable"
    );
    // The seed/main checkout is untouched: its directory and seed file remain.
    assert!(
        repo.path().join("seed.txt").exists(),
        "the main checkout is never modified"
    );
    // Prune cleared the registration: the removed worktree no longer appears in the registry.
    let listing = git_stdout(repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(
        !listing.contains("task-clean"),
        "prune cleared the removed worktree's registration: {listing}"
    );
}

// ===========================================================================================
// Task 7.5 — crash recovery: dirty retained, clean removed, outside-dir worktree untouched
// ===========================================================================================

/// Crash recovery over a cold-start manager (empty live registry): a stale dirty worktree is
/// retained for review, a stale clean one is removed, each is reported with its root/branch/flag,
/// and a worktree created OUTSIDE `worktrees_dir` is never listed or touched (Req 6.2–6.6).
#[tokio::test]
async fn recover_stale_retains_dirty_removes_clean_and_ignores_outside() {
    let repo = seed_repo();
    let wt = TempDir::new().unwrap();

    // Use a first manager to create two worktrees as direct subdirectories of worktrees_dir.
    let creator = manager(wt.path(), "Q:");
    let dirty_task = TaskId::new("stale-dirty");
    let clean_task = TaskId::new("stale-clean");
    let dirty_root = creator
        .create(&dirty_task, repo.path(), None)
        .await
        .unwrap();
    let clean_root = creator
        .create(&clean_task, repo.path(), None)
        .await
        .unwrap();

    // Make one of them dirty with an untracked file; leave the other clean.
    std::fs::write(dirty_root.join("wip.txt"), "work in progress\n").unwrap();

    // Create a worktree OUTSIDE worktrees_dir, directly via git, so recovery must ignore it.
    let outside = TempDir::new().unwrap();
    let outside_root = outside.path().join("external-wt");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "external/branch",
            &outside_root.to_string_lossy(),
        ],
    );
    assert!(outside_root.exists(), "the outside worktree was created");

    // Fresh manager = cold start (empty live registry), so every registered worktree under
    // worktrees_dir is stale.
    let recoverer = manager(wt.path(), "Q:");
    let report = recoverer.recover_stale().await;

    // Locate the two reports by their stable branch name. The clean worktree's directory is
    // removed by recovery, so canonicalizing its (now-deleted) root — or the entry's stored
    // `s.root` — would fail; the branch field needs no filesystem access and is already asserted.
    let find = |branch: &str| report.stale.iter().find(|s| s.branch == branch);

    let dirty_report = find("nebula/stale-dirty").unwrap();
    assert!(
        dirty_report.has_uncommitted_work,
        "the dirty worktree is flagged as carrying work"
    );
    assert_eq!(
        dirty_report.disposition,
        Disposition::RetainedForReview,
        "a dirty stale worktree is retained for review"
    );
    assert_eq!(
        dirty_report.branch, "nebula/stale-dirty",
        "the report carries the branch name"
    );
    assert!(
        dirty_root.exists(),
        "a retained dirty worktree is never deleted"
    );

    let clean_report = find("nebula/stale-clean").unwrap();
    assert_eq!(
        clean_report.disposition,
        Disposition::Removed,
        "a clean stale worktree within the limit is removed"
    );
    assert_eq!(
        clean_report.branch, "nebula/stale-clean",
        "the report carries the branch name"
    );
    assert!(
        !clean_root.exists(),
        "a removed clean worktree's directory is gone"
    );

    // The outside worktree is never listed and never touched.
    assert!(
        report
            .stale
            .iter()
            .all(|s| !s.root.ends_with("external-wt")),
        "recovery must not list a worktree outside worktrees_dir"
    );
    assert!(
        outside_root.exists(),
        "recovery must not touch a worktree outside worktrees_dir"
    );
}

// ===========================================================================================
// Task 10.6 — concurrent-isolation + main-checkout-unmodified integration test (not proptest)
// ===========================================================================================

/// Snapshot of the `Main_Checkout`'s observable state: its HEAD commit, checked-out branch, and
/// `git status --porcelain`. Property 1 / Requirement 4.1 require all three unchanged by any task
/// lifecycle run against the repository.
#[derive(Debug, PartialEq, Eq)]
struct MainSnapshot {
    head: String,
    branch: String,
    porcelain: String,
}

/// Capture the `Main_Checkout` snapshot (HEAD commit, branch name, working-tree status).
fn snapshot_main(repo: &Path) -> MainSnapshot {
    MainSnapshot {
        head: git_stdout(repo, &["rev-parse", "HEAD"]),
        branch: git_stdout(repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        porcelain: git_stdout(repo, &["status", "--porcelain"]),
    }
}

/// Two tasks created concurrently against one repository get distinct worktree roots and distinct
/// branches (Req 2.4, 3.1, 3.2 — manager side), and a full create → commit-in-worktree → finalize
/// → remove lifecycle on one task leaves the `Main_Checkout`'s HEAD, branch, and status byte-identical
/// (Req 4.1).
///
/// The complementary half — two concurrent `nebula_tools::CURRENT_WORKTREE.scope` tasks each
/// reading their own root and never the other's — is not reachable from a `nebula-sandbox`
/// integration test (`nebula-sandbox` does not depend on `nebula-tools`, to avoid a dependency
/// cycle). That half is covered by the `nebula-tools` provider's `concurrent_tasks_each_read_their_own_root`
/// unit test (tasks 4.1 / 4.3).
#[tokio::test]
async fn concurrent_tasks_are_distinct_and_lifecycle_leaves_main_unmodified() {
    let repo = seed_repo();
    let wt = TempDir::new().unwrap();
    let mgr = manager(wt.path(), "Q:");

    // --- Concurrent creation against one repository: distinct roots and branches. ---
    let task_a = TaskId::new("concurrent-a");
    let task_b = TaskId::new("concurrent-b");
    let (root_a, root_b) = tokio::join!(
        mgr.create(&task_a, repo.path(), None),
        mgr.create(&task_b, repo.path(), None),
    );
    let root_a = root_a.unwrap();
    let root_b = root_b.unwrap();

    assert_ne!(
        root_a, root_b,
        "concurrent tasks must get distinct worktree roots"
    );
    assert!(
        !root_a.starts_with(&root_b) && !root_b.starts_with(&root_a),
        "neither concurrent worktree root may nest under the other: {root_a:?} / {root_b:?}"
    );
    let branch_a = mgr.finalize(&task_a).await.unwrap();
    let branch_b = mgr.finalize(&task_b).await.unwrap();
    assert_ne!(
        branch_a, branch_b,
        "concurrent tasks must get distinct task branches"
    );

    // Tidy the two concurrent worktrees (both clean) so only the lifecycle task below remains.
    mgr.remove_worktree(&task_a).await.unwrap();
    mgr.remove_worktree(&task_b).await.unwrap();

    // --- Full lifecycle on one task leaves the main checkout byte-identical (Req 4.1). ---
    let before = snapshot_main(repo.path());

    let task = TaskId::new("lifecycle");
    let root = mgr.create(&task, repo.path(), None).await.unwrap();

    // Make a real commit INSIDE the worktree: write a file, stage it, commit it there.
    std::fs::write(root.join("work.txt"), "work done in the worktree\n").unwrap();
    git(&root, &["add", "work.txt"]);
    git(&root, &["commit", "-m", "work"]);

    let branch = mgr.finalize(&task).await.unwrap();
    assert_eq!(
        branch, "nebula/lifecycle",
        "finalize returns the task branch"
    );

    // The worktree now carries a committed change but a clean working tree, so removal succeeds.
    mgr.remove_worktree(&task).await.unwrap();
    assert!(
        !root.exists(),
        "remove_worktree deletes the worktree directory"
    );

    let after = snapshot_main(repo.path());
    assert_eq!(
        before, after,
        "the main checkout's HEAD, branch, and status must be unchanged by a task lifecycle"
    );
}

// ===========================================================================================
// Property tests (proptest). proptest bodies are synchronous, so each case drives the async
// manager API with a current-thread Tokio runtime via `block_on`, mirroring the in-crate
// `measure.rs` property tests. Real git is slow, so each case builds a tiny repo and runs 100
// cases per the spec minimum.
// ===========================================================================================

mod property_tests {
    use super::*;
    use proptest::prelude::*;

    /// The state a generated worktree is placed in before the invariant is checked.
    #[derive(Clone, Copy, Debug)]
    enum WorktreeState {
        /// Clean working tree, branch at the start point (no extra work).
        Clean,
        /// An untracked file present — a dirty working tree.
        DirtyUntracked,
        /// A tracked file modified — a dirty working tree.
        DirtyModified,
        /// A clean working tree but an extra commit on the task branch reachable from no other ref.
        ExtraCommit,
    }

    fn any_state() -> impl Strategy<Value = WorktreeState> {
        prop_oneof![
            Just(WorktreeState::Clean),
            Just(WorktreeState::DirtyUntracked),
            Just(WorktreeState::DirtyModified),
            Just(WorktreeState::ExtraCommit),
        ]
    }

    /// Build a current-thread Tokio runtime for a synchronous proptest case body.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// Put the worktree at `root` into `state`, returning whether the state carries uncommitted
    /// work (a dirty tree OR an unmerged commit).
    fn apply_state(root: &Path, state: WorktreeState) -> bool {
        match state {
            WorktreeState::Clean => false,
            WorktreeState::DirtyUntracked => {
                std::fs::write(root.join("untracked.txt"), "scratch\n").unwrap();
                true
            }
            WorktreeState::DirtyModified => {
                // `seed.txt` is a tracked file from the seed commit; modifying it dirties the tree.
                std::fs::write(root.join("seed.txt"), "seed\nmodified\n").unwrap();
                true
            }
            WorktreeState::ExtraCommit => {
                // Commit a new file on the task branch inside the worktree, then leave the tree
                // clean. The commit must be reachable from no other ref (unmerged work). The
                // completion and recovery worktrees share a seed repo and committer identity, so an
                // identical blob + parent + message committed within the same clock second would
                // yield the *same* commit OID — making this commit reachable from the sibling
                // branch and thus (correctly) not "unmerged". Seed the content with this worktree's
                // unique root path so the tree, and therefore the commit OID, differs per worktree.
                std::fs::write(
                    root.join("feature.txt"),
                    format!("feature\n{}\n", root.display()),
                )
                .unwrap();
                git(root, &["add", "feature.txt"]);
                git(root, &["commit", "-m", "feature work"]);
                true
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: git-worktree-per-task, Property 5: a worktree with uncommitted work is never deleted
        //
        // Property 5: A worktree with uncommitted work is never deleted.
        // Validates: Requirements 5.6, 6.4, 6.5.
        //
        // For each generated state {Clean, DirtyUntracked, DirtyModified, ExtraCommit} build a
        // worktree in that state over a fresh seeded temp repo, then exercise the cleanup paths:
        //  - Completion `remove_worktree`: a DIRTY working tree is refused (DirtyWorktree) and the
        //    directory retained. (git worktree remove does NOT refuse a clean-but-unmerged tree, so
        //    the ExtraCommit "work present" invariant is checked via crash recovery below instead.)
        //  - Crash recovery `recover_stale`: has_uncommitted_work covers BOTH a dirty tree AND
        //    unmerged commits, so ANY state carrying work is RetainedForReview and never Removed,
        //    while a Clean worktree is Removed.
        #[test]
        fn work_present_is_never_deleted(state in any_state()) {
            runtime().block_on(async {
                let repo = seed_repo();

                // --- Completion path over a dedicated worktree ---
                let cwt = TempDir::new().unwrap();
                let completion_mgr = manager(cwt.path(), "Q:");
                let ctask = TaskId::new("prop5-complete");
                let croot = completion_mgr.create(&ctask, repo.path(), None).await.unwrap();
                let has_work = apply_state(&croot, state);

                match state {
                    WorktreeState::DirtyUntracked | WorktreeState::DirtyModified => {
                        // A dirty working tree is refused and retained (Req 5.6).
                        let err = completion_mgr.remove_worktree(&ctask).await.unwrap_err();
                        prop_assert!(
                            matches!(err, WorktreeError::DirtyWorktree(_)),
                            "expected DirtyWorktree, got {err:?}"
                        );
                        prop_assert!(croot.exists(), "a dirty worktree is retained");
                    }
                    WorktreeState::Clean | WorktreeState::ExtraCommit => {
                        // A clean working tree (even with unmerged commits) is removable via the
                        // completion path; this is fine — the "work present" guarantee for unmerged
                        // commits is enforced by crash recovery, checked below.
                        completion_mgr.remove_worktree(&ctask).await.unwrap();
                        prop_assert!(!croot.exists(), "a clean worktree is removed");
                    }
                }

                // --- Crash recovery path over a dedicated worktree ---
                let rwt = TempDir::new().unwrap();
                let creator = manager(rwt.path(), "Q:");
                let rtask = TaskId::new("prop5-recover");
                let rroot = creator.create(&rtask, repo.path(), None).await.unwrap();
                let has_work_r = apply_state(&rroot, state);
                prop_assert_eq!(has_work, has_work_r, "both worktrees share the state's work flag");

                // Cold-start recovery: a fresh manager with an empty live registry.
                let recoverer = manager(rwt.path(), "Q:");
                let report = recoverer.recover_stale().await;
                // Match the entry by its stable branch name: a clean worktree is removed by
                // recovery, so canonicalizing its (now-deleted) root would fail. The branch field
                // needs no filesystem access.
                let entry = report
                    .stale
                    .iter()
                    .find(|s| s.branch == "nebula/prop5-recover")
                    .unwrap();

                if has_work_r {
                    // ANY worktree carrying work (dirty tree OR unmerged commits) is retained and
                    // never deleted (Req 6.4, 6.5).
                    prop_assert!(entry.has_uncommitted_work, "work is detected");
                    prop_assert_eq!(
                        entry.disposition.clone(),
                        Disposition::RetainedForReview,
                        "a worktree with work is retained for review"
                    );
                    prop_assert!(rroot.exists(), "a worktree with work is never deleted");
                } else {
                    // A genuinely clean worktree is eligible for removal.
                    prop_assert!(!entry.has_uncommitted_work, "no work on a clean worktree");
                    prop_assert_eq!(
                        entry.disposition.clone(),
                        Disposition::Removed,
                        "a clean stale worktree is removed"
                    );
                }
                Ok(())
            })?;
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: git-worktree-per-task, Property 9: crash recovery never touches worktrees outside Worktrees_Dir
        //
        // Property 9: Crash recovery never touches worktrees outside Worktrees_Dir.
        // Validates: Requirements 6.1, 6.2.
        //
        // Generate a small mix of worktrees inside and outside worktrees_dir (clean, so the inside
        // ones are eligible for removal), run `recover_stale`, and assert every OUTSIDE worktree
        // directory still exists untouched afterward and none appears in the report. All paths live
        // in tempdirs.
        #[test]
        fn recovery_never_touches_outside_worktrees(
            inside in 0usize..3,
            outside in 0usize..3,
        ) {
            runtime().block_on(async {
                let repo = seed_repo();
                let wt = TempDir::new().unwrap();

                // Create `inside` clean worktrees as direct subdirectories of worktrees_dir.
                let creator = manager(wt.path(), "Q:");
                for i in 0..inside {
                    let task = TaskId::new(format!("inside-{i}"));
                    creator.create(&task, repo.path(), None).await.unwrap();
                }

                // Create `outside` worktrees in separate tempdirs, directly via git.
                let mut outside_dirs: Vec<TempDir> = Vec::new();
                let mut outside_roots: Vec<PathBuf> = Vec::new();
                for i in 0..outside {
                    let d = TempDir::new().unwrap();
                    let root = d.path().join(format!("ext-{i}"));
                    git(
                        repo.path(),
                        &[
                            "worktree",
                            "add",
                            "-b",
                            &format!("external/{i}"),
                            &root.to_string_lossy(),
                        ],
                    );
                    prop_assert!(root.exists(), "the outside worktree was created");
                    outside_roots.push(root);
                    outside_dirs.push(d);
                }

                // Cold-start recovery over worktrees_dir.
                let recoverer = manager(wt.path(), "Q:");
                let report = recoverer.recover_stale().await;

                // Every outside worktree is untouched and absent from the report.
                for root in &outside_roots {
                    prop_assert!(
                        root.exists(),
                        "recovery must not delete a worktree outside worktrees_dir: {root:?}"
                    );
                    let want = root.canonicalize().unwrap();
                    let listed = report
                        .stale
                        .iter()
                        .any(|s| s.root.canonicalize().map(|r| r == want).unwrap_or(false));
                    prop_assert!(!listed, "an outside worktree must not appear in the report: {root:?}");
                }

                // Keep the tempdirs alive until here.
                drop(outside_dirs);
                Ok(())
            })?;
        }
    }

    /// A short sequence of lifecycle operations over a small task-id space, applied in order with
    /// errors ignored. Drives Property 1: whatever (valid or invalid) order a caller issues
    /// create/finalize/remove in, the `Main_Checkout` is never modified.
    #[derive(Clone, Copy, Debug)]
    enum Op {
        /// `create` a worktree for task `id` (ignores an already-active / collision error).
        Create(u8),
        /// `finalize` task `id` (ignores an unknown-task error).
        Finalize(u8),
        /// `remove_worktree` for task `id` (ignores an unknown-task / dirty error).
        Remove(u8),
    }

    /// Generate a single op over a tiny id space (ids `0..3`) so sequences repeatedly hit the same
    /// task — exercising already-active, finalize-after-create, and remove-after-finalize paths.
    fn any_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..3).prop_map(Op::Create),
            (0u8..3).prop_map(Op::Finalize),
            (0u8..3).prop_map(Op::Remove),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: git-worktree-per-task, Property 1: the main checkout is never modified
        //
        // Property 1: The main checkout is never modified.
        // Validates: Requirements 4.1, 4.2.
        //
        // Generate a short sequence (1..5) of create/finalize/remove ops over a small id space
        // against ONE seeded temp repo and ONE manager. Snapshot the Main_Checkout's HEAD commit,
        // checked-out branch, and `git status --porcelain` before applying the ops, apply every op
        // ignoring errors (invalid orderings — remove-before-create, double-create — are expected
        // and swallowed), then snapshot again and assert all three are byte-identical. No worktree
        // lifecycle operation, in any order, may touch the owner's main checkout.
        #[test]
        fn main_checkout_is_never_modified(ops in prop::collection::vec(any_op(), 1..5)) {
            runtime().block_on(async {
                let repo = seed_repo();
                let wt = TempDir::new().unwrap();
                let mgr = manager(wt.path(), "Q:");

                let before = snapshot_main(repo.path());

                for op in &ops {
                    match *op {
                        Op::Create(id) => {
                            let task = TaskId::new(format!("prop1-{id}"));
                            // Invalid sequences (already active) are expected; ignore the result.
                            let _ = mgr.create(&task, repo.path(), None).await;
                        }
                        Op::Finalize(id) => {
                            let task = TaskId::new(format!("prop1-{id}"));
                            let _ = mgr.finalize(&task).await;
                        }
                        Op::Remove(id) => {
                            let task = TaskId::new(format!("prop1-{id}"));
                            let _ = mgr.remove_worktree(&task).await;
                        }
                    }
                }

                let after = snapshot_main(repo.path());
                prop_assert_eq!(
                    before, after,
                    "the main checkout's HEAD, branch, and status must be unchanged by any \
                     sequence of worktree lifecycle operations"
                );
                Ok(())
            })?;
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: git-worktree-per-task, Property 2: worktree and branch assignments are unique across live tasks
        //
        // Property 2: Worktree and branch assignments are unique and non-overlapping across live
        // tasks.
        // Validates: Requirements 2.4, 3.1, 3.2, 3.3, 3.5, 4.4.
        //
        // Generate 2..5 distinct task ids and create them all (live at once) against one repo.
        // Assert the resulting Worktree_Roots are pairwise-distinct, the Task_Branches are
        // pairwise-distinct, and no root nests under another (`!a.starts_with(b)` both ways for
        // distinct tasks) — the sandbox-side proxy for the WorktreeEscape guarantee. The
        // complementary half, that `path::resolve(path-under-root-A, root-B, retired)` is
        // `Rejected(WorktreeEscape)`, lives in `nebula-tools` (its resolver tests, issue #27) and
        // the provider property test (task 4.2); `path::resolve` is unreachable from a
        // `nebula-sandbox` test without re-introducing the dependency cycle.
        #[test]
        fn roots_and_branches_are_unique_across_live_tasks(n in 2usize..5) {
            runtime().block_on(async {
                let repo = seed_repo();
                let wt = TempDir::new().unwrap();
                let mgr = manager(wt.path(), "Q:");

                // Create `n` distinct tasks, all live at once against the one repository.
                let mut roots: Vec<PathBuf> = Vec::new();
                let mut branches: Vec<String> = Vec::new();
                for i in 0..n {
                    let task = TaskId::new(format!("prop2-{i}"));
                    let root = mgr.create(&task, repo.path(), None).await.unwrap();
                    let branch = mgr.finalize(&task).await.unwrap();
                    roots.push(root);
                    branches.push(branch);
                }

                // Pairwise checks over every distinct ordered pair.
                for a in 0..n {
                    for b in 0..n {
                        if a == b {
                            continue;
                        }
                        prop_assert_ne!(
                            &roots[a], &roots[b],
                            "live tasks must get distinct worktree roots"
                        );
                        prop_assert_ne!(
                            &branches[a], &branches[b],
                            "live tasks must get distinct task branches"
                        );
                        // Non-nesting: neither root may lie under the other (WorktreeEscape proxy).
                        prop_assert!(
                            !roots[a].starts_with(&roots[b]),
                            "root {:?} must not nest under {:?}",
                            roots[a], roots[b]
                        );
                    }
                }
                Ok(())
            })?;
        }
    }

    /// Generate a filesystem-safe task-id leaf: a short non-empty string of lowercase letters,
    /// digits, and dashes. Used verbatim as the worktree directory name under `Worktrees_Dir`.
    fn safe_task_id() -> impl Strategy<Value = String> {
        "[a-z0-9][a-z0-9-]{0,15}"
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: git-worktree-per-task, Property 3: every created worktree is placed under Worktrees_Dir, never on the retired drive
        //
        // Property 3: Every created worktree is placed under the configured Worktrees_Dir, never on
        // the retired drive.
        // Validates: Requirements 1.3, 1.9, 8.1, 8.2, 8.3.
        //
        // Happy half: with retired_drive set to an unused letter ("Q:"), a successfully created
        // Worktree_Root is a descendant of the (canonicalized) worktrees_dir. Retired half: with
        // retired_drive set to the tempdir's OWN drive, `create` is rejected with RetiredDrive and
        // no worktree directory is created. All paths live in tempdirs; never `C:`.
        #[test]
        fn created_root_is_under_worktrees_dir_and_rejects_retired_drive(id in safe_task_id()) {
            runtime().block_on(async {
                // --- Happy half: retired drive is an unused letter, so creation proceeds. ---
                let repo = seed_repo();
                let wt = TempDir::new().unwrap();
                let mgr = manager(wt.path(), "Q:");

                let task = TaskId::new(id.clone());
                let root = mgr.create(&task, repo.path(), None).await.unwrap();

                let parent = wt.path().canonicalize().unwrap();
                prop_assert!(
                    root.canonicalize().unwrap().starts_with(&parent),
                    "created root {root:?} must be a descendant of worktrees_dir {parent:?}"
                );

                // --- Retired half: treat the tempdir's own drive as retired → rejection. ---
                let repo2 = seed_repo();
                let wt2 = TempDir::new().unwrap();
                let retired = drive_of(wt2.path());
                let mgr2 = manager(wt2.path(), &retired);

                let task2 = TaskId::new(id.clone());
                let err = mgr2.create(&task2, repo2.path(), None).await.unwrap_err();
                prop_assert!(
                    matches!(err, WorktreeError::RetiredDrive(_)),
                    "a worktrees_dir on the retired drive must be rejected, got {err:?}"
                );
                prop_assert!(
                    !wt2.path().join(&id).exists(),
                    "a retired-drive rejection must create no worktree directory"
                );
                Ok(())
            })?;
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: git-worktree-per-task, Property 8: finalize and remove retain the Task_Branch
        //
        // Property 8: Finalize and remove retain the Task_Branch.
        // Validates: Requirements 5.1, 5.4.
        //
        // For each generated task id: create a clean worktree, record the Task_Branch's commit
        // (`git rev-parse refs/heads/<branch>` in the owning repo), finalize and assert the branch
        // still resolves to that same commit, then remove_worktree and assert the branch STILL
        // resolves to the same commit while the worktree directory is gone. Neither finalize nor
        // remove may delete or move the branch.
        #[test]
        fn finalize_and_remove_retain_the_branch(id in safe_task_id()) {
            runtime().block_on(async {
                let repo = seed_repo();
                let wt = TempDir::new().unwrap();
                let mgr = manager(wt.path(), "Q:");

                let task = TaskId::new(id.clone());
                let root = mgr.create(&task, repo.path(), None).await.unwrap();
                let branch = format!("nebula/{id}");

                // The branch's commit before finalize/remove.
                let before =
                    git_stdout(repo.path(), &["rev-parse", &format!("refs/heads/{branch}")]);

                // finalize keeps the branch at the same commit.
                let returned = mgr.finalize(&task).await.unwrap();
                prop_assert_eq!(&returned, &branch, "finalize returns the task branch");
                prop_assert!(
                    branch_resolves(repo.path(), &branch),
                    "finalize retains the branch"
                );
                let after_finalize =
                    git_stdout(repo.path(), &["rev-parse", &format!("refs/heads/{branch}")]);
                prop_assert_eq!(
                    &before, &after_finalize,
                    "finalize must not move the branch"
                );

                // remove_worktree deletes only the directory and keeps the branch at its commit.
                mgr.remove_worktree(&task).await.unwrap();
                prop_assert!(!root.exists(), "remove_worktree deletes the worktree directory");
                prop_assert!(
                    branch_resolves(repo.path(), &branch),
                    "remove_worktree retains the branch"
                );
                let after_remove =
                    git_stdout(repo.path(), &["rev-parse", &format!("refs/heads/{branch}")]);
                prop_assert_eq!(
                    before, after_remove,
                    "remove_worktree must not move the branch"
                );
                Ok(())
            })?;
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: git-worktree-per-task, Property 10: a failed create leaves no state blocking a retry
        //
        // Property 10: A failed create leaves no state blocking a retry.
        // Validates: Requirements 1.7, 9.6.
        //
        // A first `create` fails via an unresolvable start-point ref (UnresolvableRef). Because the
        // ref cannot be resolved, `git worktree add` is never reached — no branch and no directory
        // are created and no live assignment is recorded — so a second `create` with the SAME task
        // id succeeds (it is NOT rejected as TaskAlreadyActive, Req 9.6). This is the reliable
        // trigger the spec prefers; it isolates the "no lingering state" guarantee from git's own
        // partial-add side effects (a failed `git worktree add -b` can leave the branch behind,
        // which is tracked separately and is out of scope for this property).
        #[test]
        fn failed_create_does_not_block_a_retry(id in safe_task_id()) {
            runtime().block_on(async {
                let repo = seed_repo();
                let wt = TempDir::new().unwrap();
                let mgr = manager(wt.path(), "Q:");

                let task = TaskId::new(id.clone());
                let bogus = format!("bogus-{id}");
                let err = mgr
                    .create(&task, repo.path(), Some(&bogus))
                    .await
                    .unwrap_err();
                prop_assert!(
                    matches!(err, WorktreeError::UnresolvableRef(ref r) if r == &bogus),
                    "expected UnresolvableRef, got {err:?}"
                );
                // Nothing was created by the failed attempt: no worktree directory for the task.
                prop_assert!(
                    !wt.path().join(&id).exists(),
                    "a failed create must leave no worktree directory"
                );
                // Same id is accepted on retry (no lingering live assignment, Req 9.6).
                let root = mgr.create(&task, repo.path(), None).await.unwrap();
                prop_assert!(root.exists(), "a retry after a failed create succeeds");
                Ok(())
            })?;
        }
    }
}
