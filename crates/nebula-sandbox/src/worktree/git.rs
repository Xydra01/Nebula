//! Internal git-shelling helper for the worktree lifecycle.
//!
//! This module is the one place the [`WorktreeManager`](crate::worktree::manager::WorktreeManager)
//! and crash-recovery reach git. It defines the internal async `run_git` mirroring the issue #27
//! `git.rs::run_git` (a [`tokio::process::Command`] with `current_dir` set, piped stdout/stderr,
//! `kill_on_drop(true)`, bounded by [`tokio::time::timeout`]), and the typed thin wrappers over the
//! git subcommands the manager and recovery need (design Section 4.4, "git commands used").
//!
//! | Operation | git command |
//! |---|---|
//! | Create worktree + branch | `git worktree add -b <branch> <path> <start-point>` |
//! | Enumerate registered worktrees | `git worktree list --porcelain` |
//! | Remove a worktree directory | `git worktree remove <path>` (refuses a dirty tree) |
//! | Clear removed registrations | `git worktree prune` |
//! | Resolve a ref | `git rev-parse --verify <ref>` |
//! | Check branch existence | `git rev-parse --verify --quiet refs/heads/<branch>` |
//! | Detect unmerged commits | `git rev-list --count <branch> --not --exclude=refs/heads/<branch> --exclude=HEAD --all` |
//! | Dirty working tree | `git status --porcelain` |
//!
//! Every invocation maps a spawn failure, a wait failure, or a timeout to the single
//! [`WorktreeError::Git`] variant so a caller never sees a raw git error (Requirement 9.5). No
//! `unwrap`/`expect`/`panic` is used here, and `kill_on_drop(true)` ensures a dropped future cannot
//! leak a running git process.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::worktree::error::WorktreeError;

/// Default timeout for a single git invocation.
///
/// A worktree git query is a fast, local operation; a run that does not finish well within this
/// bound indicates a stuck child, which [`run_git`] terminates via `kill_on_drop`. The wrappers in
/// this module pass this value to [`run_git`]; `create`'s own 30-second budget (Requirement 1.6) is
/// enforced by the manager over the create sequence as a whole.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// The captured result of a single git invocation.
///
/// [`success`](GitOutput::success) reflects the process exit status; a non-zero exit is **not**
/// itself mapped to an error by `run_git`, so a caller can distinguish an expected non-zero exit
/// (for example `rev-parse --verify --quiet` over an absent ref, or `git worktree remove` refusing
/// a dirty tree) from a hard failure. [`stdout`](GitOutput::stdout) and [`stderr`](GitOutput::stderr)
/// are the lossy-UTF-8 captured streams.
#[derive(Clone, Debug)]
pub struct GitOutput {
    /// Whether git exited successfully (status code 0).
    pub success: bool,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error, used for diagnostics in error messages.
    pub stderr: String,
}

/// Run `git <args...>` with the working directory set to `repo_or_wt`, bounded by `timeout`.
///
/// The child's stdin is null, its stdout/stderr are piped and captured, and `kill_on_drop(true)` is
/// set so that if the returned future is dropped (including on timeout) the git process is
/// terminated rather than leaked. `current_dir` is the target repository for create / list / prune
/// / ref queries, or the worktree directory for a `status --porcelain` check (design Section 4.4).
///
/// A non-zero exit is reported through [`GitOutput::success`] rather than as an error, so callers
/// can distinguish an expected non-zero exit from a hard failure.
///
/// # Errors
/// Returns [`WorktreeError::Git`] if git cannot be spawned (for example git is not installed), if
/// waiting on it fails, or if it does not exit before `timeout` elapses.
async fn run_git(
    repo_or_wt: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<GitOutput, WorktreeError> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(repo_or_wt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| WorktreeError::Git(format!("could not start git: {e}")))?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(GitOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        Ok(Err(e)) => Err(WorktreeError::Git(format!("waiting on git failed: {e}"))),
        // Timed out: the child future is dropped here, and `kill_on_drop(true)` terminates git.
        Err(_elapsed) => Err(WorktreeError::Git(format!(
            "git did not complete within {}s",
            timeout.as_secs()
        ))),
    }
}

/// Map a completed [`GitOutput`] to an error when it exited non-zero, labelling the failure with
/// `operation` and the trimmed stderr.
///
/// # Errors
/// Returns [`WorktreeError::Git`] when `out.success` is false.
fn require_success(out: GitOutput, operation: &str) -> Result<GitOutput, WorktreeError> {
    if out.success {
        Ok(out)
    } else {
        Err(WorktreeError::Git(format!(
            "git {operation} failed: {}",
            out.stderr.trim()
        )))
    }
}

/// Whether `repo` is inside a git working tree, via `git rev-parse --is-inside-work-tree`.
///
/// `git rev-parse --is-inside-work-tree` prints `true` and exits 0 inside a work tree, and exits
/// non-zero (writing a `fatal: not a git repository` diagnostic to stderr) outside one. A clean
/// non-zero exit is therefore the ordinary "not a repository" answer and is mapped to `Ok(false)`,
/// not to an error — the caller turns `Ok(false)` into [`WorktreeError::NotARepository`] with the
/// offending path (Requirement 9.2).
///
/// # Errors
/// Returns [`WorktreeError::Git`] if git fails to spawn or times out.
pub async fn is_git_repo(repo: &Path) -> Result<bool, WorktreeError> {
    let out = run_git(repo, &["rev-parse", "--is-inside-work-tree"], GIT_TIMEOUT).await?;
    Ok(out.success && out.stdout.trim() == "true")
}

/// `git worktree add -b <branch> <path> <start_point>` in `repo`: create a new worktree at `path`
/// checking out a newly created `branch` based on `start_point` (design Section 4.4; Requirements
/// 1.1, 1.4).
///
/// The caller resolves `start_point` and confirms `branch` does not already exist beforehand; this
/// wrapper only shells the command and requires a successful exit.
///
/// # Errors
/// Returns [`WorktreeError::Git`] if git fails to spawn, times out, or exits non-zero (for example
/// the branch already exists or the start-point is unresolvable at the git level).
pub async fn worktree_add(
    repo: &Path,
    branch: &str,
    path: &Path,
    start_point: &str,
) -> Result<GitOutput, WorktreeError> {
    let path_str = path.to_string_lossy();
    let out = run_git(
        repo,
        &["worktree", "add", "-b", branch, &path_str, start_point],
        GIT_TIMEOUT,
    )
    .await?;
    require_success(out, "worktree add")
}

/// `git worktree list --porcelain` in `repo`: enumerate the registered worktrees in the stable
/// porcelain format for recovery to parse (design Section 4.4; Requirement 6.1).
///
/// # Errors
/// Returns [`WorktreeError::Git`] if git fails to spawn, times out, or exits non-zero.
pub async fn worktree_list_porcelain(repo: &Path) -> Result<GitOutput, WorktreeError> {
    let out = run_git(repo, &["worktree", "list", "--porcelain"], GIT_TIMEOUT).await?;
    require_success(out, "worktree list --porcelain")
}

/// `git worktree remove <path>` in `repo`: remove a worktree's directory and registration (design
/// Section 4.4; Requirements 5.2, 6.6).
///
/// git **refuses to remove a dirty worktree**; this wrapper surfaces the raw [`GitOutput`] (it does
/// **not** require success) so the caller can map a non-zero exit arising from a dirty tree to
/// [`WorktreeError::DirtyWorktree`] rather than a generic failure.
///
/// # Errors
/// Returns [`WorktreeError::Git`] only if git fails to spawn or times out; a non-zero exit is
/// returned via [`GitOutput::success`] for the caller to classify.
pub async fn worktree_remove(repo: &Path, path: &Path) -> Result<GitOutput, WorktreeError> {
    let path_str = path.to_string_lossy();
    run_git(repo, &["worktree", "remove", &path_str], GIT_TIMEOUT).await
}

/// `git worktree prune` in `repo`: clear the registrations of worktrees whose directories were
/// removed (design Section 4.4; Requirements 5.3, 6.6).
///
/// # Errors
/// Returns [`WorktreeError::Git`] if git fails to spawn, times out, or exits non-zero.
pub async fn worktree_prune(repo: &Path) -> Result<GitOutput, WorktreeError> {
    let out = run_git(repo, &["worktree", "prune"], GIT_TIMEOUT).await?;
    require_success(out, "worktree prune")
}

/// `git rev-parse --verify <reference>` in `repo`: resolve `reference` to a commit, used to resolve
/// a specified start-point ref or the default-branch HEAD (design Section 4.4; Requirements 1.1,
/// 1.2).
///
/// Returns the resolved object id (trimmed stdout) on success. A ref that cannot be resolved exits
/// non-zero and maps to [`WorktreeError::UnresolvableRef`], so the caller can reject creation
/// without creating anything (Requirement 1.2).
///
/// # Errors
/// - Returns [`WorktreeError::UnresolvableRef`] if `reference` does not resolve.
/// - Returns [`WorktreeError::Git`] if git fails to spawn or times out.
pub async fn rev_parse_verify(repo: &Path, reference: &str) -> Result<String, WorktreeError> {
    let out = run_git(repo, &["rev-parse", "--verify", reference], GIT_TIMEOUT).await?;
    if out.success {
        Ok(out.stdout.trim().to_owned())
    } else {
        Err(WorktreeError::UnresolvableRef(reference.to_owned()))
    }
}

/// Whether a local branch `branch` exists in `repo`, via
/// `git rev-parse --verify --quiet refs/heads/<branch>` (design Section 4.4; Requirement 1.5).
///
/// `--verify --quiet` exits 0 when the ref exists and non-zero (with no diagnostic) when it does
/// not, so a non-zero exit here is the ordinary "no such branch" answer rather than a failure.
///
/// # Errors
/// Returns [`WorktreeError::Git`] if git fails to spawn or times out.
pub async fn branch_exists(repo: &Path, branch: &str) -> Result<bool, WorktreeError> {
    let refname = format!("refs/heads/{branch}");
    let out = run_git(
        repo,
        &["rev-parse", "--verify", "--quiet", &refname],
        GIT_TIMEOUT,
    )
    .await?;
    Ok(out.success)
}

/// Return the `git status --porcelain` output for the worktree at `worktree_root`.
///
/// An empty string means a clean working tree; non-empty output means tracked modifications,
/// staged changes, or non-ignored untracked files are present. Callers use emptiness as the
/// dirty-tree half of the `Uncommitted_Work` predicate (Requirement 6.4).
///
/// # Errors
/// Returns [`WorktreeError::Git`] if git cannot be run, times out, or exits non-zero (for example
/// when `worktree_root` is not inside a git work tree).
pub async fn status_porcelain(worktree_root: &Path) -> Result<String, WorktreeError> {
    let out = run_git(worktree_root, &["status", "--porcelain"], GIT_TIMEOUT).await?;
    let out = require_success(out, "status --porcelain")?;
    Ok(out.stdout)
}

/// Return the number of commits on `branch` that are reachable from no *other* ref.
///
/// Implemented via
/// `git rev-list --count <branch> --not --exclude=refs/heads/<branch> --exclude=HEAD --all`
/// run with `current_dir` set to `repo_or_wt`, counting commits on `branch` that exist nowhere
/// else (unmerged / unpushed work). A count greater than zero is the unmerged-commits half of the
/// `Uncommitted_Work` predicate (Requirement 6.4). `repo_or_wt` may be the main repository or a
/// worktree directory — both share the repository object store, so the query sees every ref either
/// way.
///
/// The two `--exclude` globs (which must precede `--all` to apply to it) drop `branch` itself and
/// every per-worktree `HEAD` from the "everything else" set `--all` expands to. Without them the
/// branch's own commits are always reachable from `--all` — directly via `refs/heads/<branch>`, and
/// via the `HEAD` of the worktree that currently has `branch` checked out — so the count would
/// always be zero and a worktree carrying unmerged work would be mis-reported as clean and deleted
/// (Requirements 6.4, 6.5). Excluding both makes `--not … --all` mean "not reachable from any ref
/// except `branch` itself", so only commits unique to `branch` are counted.
///
/// # Errors
/// Returns [`WorktreeError::Git`] if git cannot be run, times out, exits non-zero, or produces a
/// count that cannot be parsed as a number.
pub async fn unmerged_commit_count(repo_or_wt: &Path, branch: &str) -> Result<u64, WorktreeError> {
    let exclude_branch = format!("--exclude=refs/heads/{branch}");
    let out = run_git(
        repo_or_wt,
        &[
            "rev-list",
            "--count",
            branch,
            "--not",
            &exclude_branch,
            "--exclude=HEAD",
            "--all",
        ],
        GIT_TIMEOUT,
    )
    .await?;
    let out = require_success(out, "rev-list --count")?;
    out.stdout.trim().parse::<u64>().map_err(|e| {
        WorktreeError::Git(format!(
            "could not parse unmerged commit count {:?} for branch {branch:?}: {e}",
            out.stdout.trim()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    /// Run `git <args>` in `dir`, asserting success. Used to arrange a real test repository exactly
    /// as the issue #27 `git.rs` tests do (`git init`, a committer identity, a seed commit).
    fn git(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("spawn git");
        assert!(st.success(), "git {args:?} failed");
    }

    /// Create a fresh initialized repo with a committer identity configured locally and a single
    /// seed commit, so a HEAD exists for ref resolution.
    fn seed_repo() -> TempDir {
        let d = TempDir::new().expect("tempdir");
        git(d.path(), &["init"]);
        git(d.path(), &["config", "user.email", "t@example.com"]);
        git(d.path(), &["config", "user.name", "t"]);
        std::fs::write(d.path().join("seed.txt"), "seed\n").expect("write seed file");
        git(d.path(), &["add", "seed.txt"]);
        git(d.path(), &["commit", "-m", "seed"]);
        d
    }

    /// `rev_parse_verify(repo, "HEAD")` resolves to a non-empty object id over a seeded repo
    /// (Requirement 9.5): the seed commit gives HEAD a resolvable target.
    #[tokio::test]
    async fn rev_parse_verify_resolves_head() {
        let repo = seed_repo();
        let oid = rev_parse_verify(repo.path(), "HEAD")
            .await
            .expect("HEAD resolves over a seeded repo");
        assert!(!oid.is_empty(), "resolved object id should be non-empty");
        // A git object id is a lowercase hex digest; sanity-check the shape.
        assert!(
            oid.chars().all(|c| c.is_ascii_hexdigit()),
            "object id {oid:?} should be hex"
        );
    }

    /// `branch_exists` is `Ok(false)` for an absent branch and `Ok(true)` after the branch is
    /// created (Requirement 9.5): the `--verify --quiet` form answers existence without erroring.
    #[tokio::test]
    async fn branch_exists_tracks_branch_creation() {
        let repo = seed_repo();
        assert!(
            !branch_exists(repo.path(), "nope")
                .await
                .expect("branch_exists query succeeds for an absent branch"),
            "an absent branch should not exist"
        );

        git(repo.path(), &["branch", "foo"]);

        assert!(
            branch_exists(repo.path(), "foo")
                .await
                .expect("branch_exists query succeeds for a present branch"),
            "the created branch should exist"
        );
    }

    /// A bogus ref maps to [`WorktreeError::UnresolvableRef`] rather than succeeding or leaking a
    /// raw git error (Requirements 1.2, 9.5); nothing is created by the query.
    #[tokio::test]
    async fn rev_parse_verify_bogus_ref_is_unresolvable() {
        let repo = seed_repo();
        let err = rev_parse_verify(repo.path(), "does-not-exist")
            .await
            .expect_err("a bogus ref must not resolve");
        assert!(
            matches!(err, WorktreeError::UnresolvableRef(ref r) if r == "does-not-exist"),
            "expected UnresolvableRef for a bogus ref, got {err:?}"
        );
    }

    /// Over a directory that is not a git repository, a wrapper requiring a successful exit maps the
    /// non-zero git exit to [`WorktreeError::Git`] (Requirement 9.5). No real path is touched: the
    /// directory is an empty `tempfile` dir that was never `git init`ed.
    #[tokio::test]
    async fn non_repo_dir_maps_to_git_error() {
        let non_repo = TempDir::new().expect("tempdir");

        // `worktree list --porcelain` requires success, so a non-repo directory is a hard failure.
        let err = worktree_list_porcelain(non_repo.path())
            .await
            .expect_err("listing worktrees outside a repository must fail");
        assert!(
            matches!(err, WorktreeError::Git(_)),
            "expected WorktreeError::Git for a non-repo dir, got {err:?}"
        );

        // `status --porcelain` likewise requires success and maps a non-repo dir to Git.
        let err = status_porcelain(non_repo.path())
            .await
            .expect_err("status outside a repository must fail");
        assert!(
            matches!(err, WorktreeError::Git(_)),
            "expected WorktreeError::Git from status in a non-repo dir, got {err:?}"
        );
    }
}
