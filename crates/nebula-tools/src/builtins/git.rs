//! The `git.*` built-in tools (Requirement 3), confined to the worktree repository.
//!
//! These tools shell out to the `git` CLI with `current_dir(worktree_root)` (design 4: git CLI
//! primary) and never operate on any repository outside the [`Worktree_Root`](crate::builtins::WorktreeRootProvider)
//! (Requirement 3.2). To share process plumbing and isolation with `shell.run`, every git
//! invocation runs through [`run_git`], which reuses the Job-Object-backed child mechanism
//! ([`crate::launcher::child::spawn_in_job`] / [`JobChild`](crate::launcher::child::JobChild)):
//! each child is placed in a kill-on-close Job Object so a cancelled (timed-out) call cannot leak
//! a running git process, and the child's output is captured for the host to cap.
//!
//! The four tools:
//!
//! - [`GitStatus`] (`git.status {}`) — working-tree status, [`Tier::Read`] (Requirement 3.6, 3.14).
//! - [`GitDiff`] (`git.diff {}`) — the diff, output flows through the host's cap, [`Tier::Read`]
//!   (Requirement 3.7, 3.14, 3.16).
//! - [`GitCommit`] (`git.commit { message }`) — commit the staged changes, [`Tier::Sandbox`]
//!   (Requirement 3.8, 3.15).
//! - [`GitBranch`] (`git.branch { action, name? }`) — list or create a branch, [`Tier::Sandbox`]
//!   for create (Requirement 3.11, 3.15).
//!
//! There is no push surface: the input schemas expose no push/fetch/pull action, and any
//! remote-mutating request is refused with [`ToolError::InvalidArguments`] without contacting any
//! remote (Requirement 3.4, 3.5). A missing repository at the worktree root maps to
//! [`ToolError::Unavailable`] (Requirement 3.3); any other git failure also maps to
//! [`ToolError::Unavailable`], leaving the repository unmodified (Requirement 3.13).

use std::ffi::OsStr;

use crate::ToolError;
use crate::builtins::{BuiltinTool, ToolContext, ToolOutput};
use crate::permit::Tier;

/// The telemetry/error `server` label shared by every built-in.
const BUILTIN: &str = "builtin";

/// Build an [`ToolError::Unavailable`] for the `builtin` server with `detail`.
fn unavailable(detail: impl Into<String>) -> ToolError {
    ToolError::Unavailable {
        server: BUILTIN.to_owned(),
        detail: detail.into(),
    }
}

/// Build an [`ToolError::InvalidArguments`] for `tool` with `detail`.
fn invalid(tool: &str, detail: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: tool.to_owned(),
        detail: detail.into(),
    }
}

/// A completed git invocation: its exit status and captured streams.
///
/// Produced by [`run_git`]. The caller inspects [`success`](GitResult::success) and the captured
/// output to decide the tool's result; a non-zero exit is not itself mapped to an error here so
/// that callers can distinguish, e.g., "no staged changes" (expected exit 1) from a hard failure.
struct GitResult {
    /// Whether git exited successfully (status code 0).
    success: bool,
    /// Captured standard output.
    stdout: Vec<u8>,
    /// Captured standard error, used for diagnostics in error details.
    stderr: Vec<u8>,
}

impl GitResult {
    /// The captured stdout as a lossy UTF-8 string.
    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// The captured stderr as a trimmed, lossy UTF-8 string, for error details.
    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }
}

/// Run `git <args...>` with the working directory set to the worktree root, inside a kill-on-close
/// Job Object, enforcing the per-call timeout.
///
/// This is the one place the `git.*` tools reach the OS. It reuses the shared child mechanism
/// ([`spawn_in_job`](crate::launcher::child::spawn_in_job)) so a cancelled call cannot leak a git
/// process, and captures stdout/stderr for the caller.
///
/// # Errors
/// - [`ToolError::Unavailable`] if git cannot be spawned (e.g. git is not installed) or waiting on
///   it fails.
/// - [`ToolError::Timeout`] if git does not exit before [`ToolContext::limits`]'s `call_timeout`
///   elapses; the child is terminated via the Job Object when the returned future is dropped.
#[cfg(windows)]
async fn run_git<I, S>(args: I, tool: &str, ctx: &ToolContext) -> Result<GitResult, ToolError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    use std::process::Stdio;

    use tokio::process::Command;

    let worktree_root = ctx.worktree.worktree_root();
    let timeout = ctx.limits.call_timeout;

    // Collect args once so the command can be rebuilt for the fallback spawn path.
    let args: Vec<std::ffi::OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
    let build_cmd = || {
        let mut cmd = Command::new("git");
        cmd.args(&args)
            .current_dir(&worktree_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    };

    // Prefer the current task's Job Object (issue #30): a `git.*` child joins the task's one job
    // when the executor has scoped it, so cancelling the task or losing the daemon kills it. With
    // no task in scope (a direct `git.*` call in a unit test, or a non-task caller) fall back to a
    // fresh per-process kill-on-close job (issue #27). Both are kill-on-close, so a timed-out call
    // never leaks a git process either way.
    let outcome = match crate::builtins::spawn_in_task_job(&mut build_cmd()) {
        Ok(mut child) => child.wait_with_timeout(timeout).await,
        Err(nebula_sandbox::task_job::JobError::NoTaskInScope) => {
            let mut child = crate::launcher::child::spawn_in_job(&mut build_cmd())
                .map_err(|e| unavailable(format!("could not start git: {e}")))?;
            child.wait_with_timeout(timeout).await
        }
        Err(e) => return Err(unavailable(format!("could not start git: {e}"))),
    };

    match outcome {
        Ok(Some(output)) => Ok(GitResult {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        }),
        Ok(None) => Err(ToolError::Timeout {
            tool: tool.to_owned(),
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        }),
        Err(e) => Err(unavailable(format!("waiting on git failed: {e}"))),
    }
}

/// Non-Windows fallback: the Job-Object child mechanism is Windows-only, so run git with the plain
/// Tokio child here. The crate targets Windows; this keeps it building elsewhere for tooling.
///
/// # Errors
/// Same mapping as the Windows implementation: [`ToolError::Unavailable`] on spawn/wait failure,
/// [`ToolError::Timeout`] when the per-call timeout elapses.
#[cfg(not(windows))]
async fn run_git<I, S>(args: I, tool: &str, ctx: &ToolContext) -> Result<GitResult, ToolError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    use std::process::Stdio;

    use tokio::process::Command;

    let worktree_root = ctx.worktree.worktree_root();
    let timeout = ctx.limits.call_timeout;

    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(&worktree_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| unavailable(format!("could not start git: {e}")))?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(GitResult {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        }),
        Ok(Err(e)) => Err(unavailable(format!("waiting on git failed: {e}"))),
        Err(_elapsed) => Err(ToolError::Timeout {
            tool: tool.to_owned(),
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        }),
    }
}

/// Confirm the worktree root contains a git repository, mapping its absence to
/// [`ToolError::Unavailable`] (Requirement 3.3).
///
/// Runs `git rev-parse --is-inside-work-tree`: a successful exit with `true` means a repository is
/// present; any other outcome (git error, not a work tree) is a missing/unusable repository.
///
/// # Errors
/// [`ToolError::Unavailable`] when the worktree root is not inside a git work tree, or when git
/// itself cannot be run; propagates [`ToolError::Timeout`] from [`run_git`].
async fn ensure_repo(tool: &str, ctx: &ToolContext) -> Result<(), ToolError> {
    let result = run_git(["rev-parse", "--is-inside-work-tree"], tool, ctx).await?;
    if result.success && result.stdout_text().trim() == "true" {
        Ok(())
    } else {
        Err(unavailable(
            "no git repository at the worktree root".to_owned(),
        ))
    }
}

/// The `git.status` built-in tool (Requirement 3.6).
///
/// Returns the working-tree status of the worktree-root repository. Classified at [`Tier::Read`]
/// (Requirement 3.14): it only reads repository state.
#[derive(Clone, Copy, Debug, Default)]
pub struct GitStatus;

impl GitStatus {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "git.status";
}

#[async_trait::async_trait]
impl BuiltinTool for GitStatus {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some("Show the working-tree status of the worktree repository.")
    }

    /// Empty-object schema: the tool takes no arguments. `additionalProperties: false` rejects any
    /// supplied field at the boundary, so no push/remote field can ever be passed (Requirement 3.4).
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn tier(&self) -> Tier {
        Tier::Read
    }

    /// Return the working-tree status.
    ///
    /// # Errors
    /// [`ToolError::Unavailable`] if the worktree root has no repository (Requirement 3.3) or git
    /// fails for any other reason (Requirement 3.13); [`ToolError::Timeout`] on per-call timeout.
    async fn call(
        &self,
        _arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        ensure_repo(Self::NAME, ctx).await?;
        let result = run_git(["status"], Self::NAME, ctx).await?;
        if result.success {
            Ok(ToolOutput::text(result.stdout_text()))
        } else {
            Err(unavailable(format!(
                "git status failed: {}",
                result.stderr_text()
            )))
        }
    }
}

/// The `git.diff` built-in tool (Requirement 3.7).
///
/// Returns the diff of the worktree-root repository. The output flows through the host's output
/// cap unchanged (Requirement 3.16): the tool emits the full diff and the boundary truncates it if
/// it exceeds the cap. Classified at [`Tier::Read`] (Requirement 3.14).
#[derive(Clone, Copy, Debug, Default)]
pub struct GitDiff;

impl GitDiff {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "git.diff";
}

#[async_trait::async_trait]
impl BuiltinTool for GitDiff {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some("Show the diff of the worktree repository.")
    }

    /// Empty-object schema: the tool takes no arguments; `additionalProperties: false` leaves no
    /// room for a remote-mutating field (Requirement 3.4).
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn tier(&self) -> Tier {
        Tier::Read
    }

    /// Return the diff.
    ///
    /// # Errors
    /// [`ToolError::Unavailable`] if the worktree root has no repository (Requirement 3.3) or git
    /// fails for any other reason (Requirement 3.13); [`ToolError::Timeout`] on per-call timeout.
    async fn call(
        &self,
        _arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        ensure_repo(Self::NAME, ctx).await?;
        let result = run_git(["diff"], Self::NAME, ctx).await?;
        if result.success {
            Ok(ToolOutput::text(result.stdout_text()))
        } else {
            Err(unavailable(format!(
                "git diff failed: {}",
                result.stderr_text()
            )))
        }
    }
}

/// The `git.commit` built-in tool (Requirement 3.8).
///
/// Commits the staged changes of the worktree-root repository with the supplied message.
/// Classified at [`Tier::Sandbox`] (Requirement 3.15): it writes repository state confined to the
/// worktree.
#[derive(Clone, Copy, Debug, Default)]
pub struct GitCommit;

impl GitCommit {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "git.commit";
}

#[async_trait::async_trait]
impl BuiltinTool for GitCommit {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some("Commit the staged changes of the worktree repository with a message.")
    }

    /// Schema requiring a single `message` string. `additionalProperties: false` rejects any other
    /// field (there is no push/remote field — Requirement 3.4).
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The commit message."
                }
            },
            "required": ["message"],
            "additionalProperties": false
        })
    }

    fn tier(&self) -> Tier {
        Tier::Sandbox
    }

    /// Commit the staged changes.
    ///
    /// Validates the message is non-empty after trimming whitespace (Requirement 3.9), confirms a
    /// repository exists (Requirement 3.3), and requires at least one staged change
    /// (Requirement 3.10) before committing.
    ///
    /// # Errors
    /// - [`ToolError::InvalidArguments`] if `message` is missing or whitespace-only
    ///   (Requirement 3.9); no commit is created.
    /// - [`ToolError::Unavailable`] if the worktree root has no repository (Requirement 3.3), there
    ///   are no staged changes (Requirement 3.10), or git fails for any other reason
    ///   (Requirement 3.13).
    /// - [`ToolError::Timeout`] on per-call timeout.
    async fn call(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        // The schema guarantees a string `message`, but defensively treat anything else as empty.
        let message = arguments
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if message.trim().is_empty() {
            return Err(invalid(
                Self::NAME,
                "commit message must not be empty or whitespace",
            ));
        }

        ensure_repo(Self::NAME, ctx).await?;

        // `git diff --cached --quiet` exits 0 when there is nothing staged, 1 when there is.
        let staged = run_git(["diff", "--cached", "--quiet"], Self::NAME, ctx).await?;
        if staged.success {
            return Err(unavailable("no staged changes to commit".to_owned()));
        }

        let result = run_git(["commit", "-m", message], Self::NAME, ctx).await?;
        if result.success {
            Ok(ToolOutput::text(result.stdout_text()))
        } else {
            Err(unavailable(format!(
                "git commit failed: {}",
                result.stderr_text()
            )))
        }
    }
}

/// The `git.branch` built-in tool (Requirement 3.11).
///
/// Lists branches, or creates a new branch with a valid, non-existing name. Create is classified
/// at [`Tier::Sandbox`] (Requirement 3.15); listing is a read, but the tool is registered at the
/// sandbox tier for its create capability.
#[derive(Clone, Copy, Debug, Default)]
pub struct GitBranch;

impl GitBranch {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "git.branch";
}

#[async_trait::async_trait]
impl BuiltinTool for GitBranch {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some("List branches, or create a new branch in the worktree repository.")
    }

    /// Schema with a required `action` of `"list"` or `"create"` and an optional `name` string.
    /// `additionalProperties: false` and the `enum` on `action` leave no room for a push/remote
    /// action (Requirement 3.4).
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "create"],
                    "description": "Whether to list branches or create a new one."
                },
                "name": {
                    "type": "string",
                    "description": "The branch name to create (required when action is \"create\")."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn tier(&self) -> Tier {
        Tier::Sandbox
    }

    /// List or create a branch.
    ///
    /// # Errors
    /// - [`ToolError::InvalidArguments`] for an unknown action, a create without a name, or a
    ///   create whose name is invalid (validated with `git check-ref-format --branch`) or already
    ///   exists (Requirement 3.12); no branch is created or modified.
    /// - [`ToolError::Unavailable`] if the worktree root has no repository (Requirement 3.3) or git
    ///   fails for any other reason (Requirement 3.13).
    /// - [`ToolError::Timeout`] on per-call timeout.
    async fn call(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let action = arguments
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        ensure_repo(Self::NAME, ctx).await?;

        match action {
            "list" => {
                let result = run_git(["branch"], Self::NAME, ctx).await?;
                if result.success {
                    Ok(ToolOutput::text(result.stdout_text()))
                } else {
                    Err(unavailable(format!(
                        "git branch failed: {}",
                        result.stderr_text()
                    )))
                }
            }
            "create" => {
                let name = arguments
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if name.is_empty() {
                    return Err(invalid(
                        Self::NAME,
                        "a branch name is required to create a branch",
                    ));
                }

                // Validate the name shape with git itself (Requirement 3.12). A leading '-' could
                // otherwise be read as an option; `--` is not accepted by check-ref-format, so a
                // name beginning with '-' is rejected here before any ref operation.
                if name.starts_with('-') {
                    return Err(invalid(
                        Self::NAME,
                        format!("invalid branch name: {name:?}"),
                    ));
                }
                let valid =
                    run_git(["check-ref-format", "--branch", name], Self::NAME, ctx).await?;
                if !valid.success {
                    return Err(invalid(
                        Self::NAME,
                        format!("invalid branch name: {name:?}"),
                    ));
                }

                // Reject an existing branch (Requirement 3.12): `rev-parse --verify` on the ref
                // succeeds only when it already exists.
                let exists = run_git(
                    [
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{name}"),
                    ],
                    Self::NAME,
                    ctx,
                )
                .await?;
                if exists.success {
                    return Err(invalid(
                        Self::NAME,
                        format!("branch already exists: {name:?}"),
                    ));
                }

                let result = run_git(["branch", "--", name], Self::NAME, ctx).await?;
                if result.success {
                    Ok(ToolOutput::text(format!("created branch {name}")))
                } else {
                    Err(unavailable(format!(
                        "git branch create failed: {}",
                        result.stderr_text()
                    )))
                }
            }
            other => Err(invalid(
                Self::NAME,
                format!("unsupported git.branch action: {other:?}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::{BuiltinLimits, ResourceProvider, WorktreeRootProvider};
    use crate::permit::DefaultClassifier;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    /// A [`WorktreeRootProvider`] that always returns a fixed path (a test temp repo).
    struct FixedWorktree(std::path::PathBuf);
    impl WorktreeRootProvider for FixedWorktree {
        fn worktree_root(&self) -> std::path::PathBuf {
            self.0.clone()
        }
    }

    /// A [`ResourceProvider`] that never has a snapshot; git tools never read it.
    struct NoResources;
    impl ResourceProvider for NoResources {
        fn latest(&self) -> Option<nebula_proto::ResourceSnapshot> {
            None
        }
    }

    /// Build a [`ToolContext`] confined to `root` with generous per-call limits.
    fn ctx_for(root: &std::path::Path) -> ToolContext {
        ToolContext {
            worktree: Arc::new(FixedWorktree(root.to_path_buf())),
            classifier: Arc::new(DefaultClassifier),
            resources: Arc::new(NoResources),
            retired_drive: "Q:".to_owned(),
            limits: BuiltinLimits {
                call_timeout: Duration::from_secs(30),
                output_cap: 65_536,
            },
        }
    }

    /// Run `git <args>` in `dir`, asserting success. Used to arrange test repositories.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?} failed");
    }

    /// Run `git <args>` in `dir`, returning whether it succeeded (does not assert).
    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    }

    /// Create a fresh initialized repo with a committer identity configured.
    fn init_repo() -> TempDir {
        let d = TempDir::new().unwrap();
        git(d.path(), &["init"]);
        git(d.path(), &["config", "user.email", "t@example.com"]);
        git(d.path(), &["config", "user.name", "t"]);
        d
    }

    /// Write a file inside the repo and stage it.
    fn write_and_stage(dir: &std::path::Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
        git(dir, &["add", name]);
    }

    /// Seed an initial commit so operations needing a HEAD (e.g. branch create) work.
    fn seed_commit(dir: &std::path::Path) {
        write_and_stage(dir, "seed.txt", "seed\n");
        git(dir, &["commit", "-m", "seed"]);
    }

    // status/diff of a dirty repo both succeed. Requirements 3.6, 3.7.
    #[tokio::test]
    async fn status_and_diff_dirty_repo_ok() {
        let d = init_repo();
        seed_commit(d.path());
        // Make a tracked change so `git diff` has content, and an untracked file for status.
        std::fs::write(d.path().join("seed.txt"), "changed\n").unwrap();
        std::fs::write(d.path().join("new.txt"), "new\n").unwrap();

        let ctx = ctx_for(d.path());
        GitStatus
            .call(serde_json::json!({}), &ctx)
            .await
            .expect("status on a dirty repo should succeed");
        GitDiff
            .call(serde_json::json!({}), &ctx)
            .await
            .expect("diff on a dirty repo should succeed");
    }

    // Committing staged changes advances HEAD. Requirement 3.8.
    #[tokio::test]
    async fn commit_staged_advances_head() {
        let d = init_repo();
        seed_commit(d.path());
        write_and_stage(d.path(), "a.txt", "hello\n");

        GitCommit
            .call(serde_json::json!({ "message": "msg" }), &ctx_for(d.path()))
            .await
            .expect("commit of staged changes should succeed");

        assert!(
            git_ok(d.path(), &["rev-parse", "HEAD"]),
            "HEAD should resolve after a commit"
        );
    }

    // Committing with nothing staged is Unavailable. Requirement 3.10.
    #[tokio::test]
    async fn commit_no_staged_changes_is_unavailable() {
        let d = init_repo();
        seed_commit(d.path());

        let err = GitCommit
            .call(serde_json::json!({ "message": "msg" }), &ctx_for(d.path()))
            .await
            .expect_err("committing with nothing staged should fail");
        assert!(
            matches!(err, ToolError::Unavailable { .. }),
            "expected Unavailable, got {err:?}"
        );
    }

    // A whitespace-only message is rejected as InvalidArguments. Requirement 3.9.
    #[tokio::test]
    async fn commit_whitespace_message_is_invalid_arguments() {
        let d = init_repo();
        seed_commit(d.path());
        write_and_stage(d.path(), "a.txt", "hello\n");

        let err = GitCommit
            .call(
                serde_json::json!({ "message": "   \t\n" }),
                &ctx_for(d.path()),
            )
            .await
            .expect_err("a whitespace-only message should be rejected");
        assert!(
            matches!(err, ToolError::InvalidArguments { .. }),
            "expected InvalidArguments, got {err:?}"
        );
    }

    // Listing branches succeeds, and creating `feature/x` succeeds and the branch then exists.
    // Requirements 3.11, 3.12.
    #[tokio::test]
    async fn branch_list_and_create_ok() {
        let d = init_repo();
        seed_commit(d.path());
        let ctx = ctx_for(d.path());

        GitBranch
            .call(serde_json::json!({ "action": "list" }), &ctx)
            .await
            .expect("listing branches should succeed");

        GitBranch
            .call(
                serde_json::json!({ "action": "create", "name": "feature/x" }),
                &ctx,
            )
            .await
            .expect("creating a valid branch should succeed");

        assert!(
            git_ok(d.path(), &["branch", "--list", "feature/x"]),
            "the created branch should exist"
        );
    }

    // An invalid branch name is InvalidArguments. Requirement 3.12.
    #[tokio::test]
    async fn branch_create_invalid_name_is_invalid_arguments() {
        let d = init_repo();
        seed_commit(d.path());

        let err = GitBranch
            .call(
                serde_json::json!({ "action": "create", "name": "bad..name" }),
                &ctx_for(d.path()),
            )
            .await
            .expect_err("an invalid branch name should be rejected");
        assert!(
            matches!(err, ToolError::InvalidArguments { .. }),
            "expected InvalidArguments, got {err:?}"
        );
    }

    // Creating an existing branch a second time is InvalidArguments. Requirement 3.12.
    #[tokio::test]
    async fn branch_create_existing_name_twice_is_invalid_arguments() {
        let d = init_repo();
        seed_commit(d.path());
        let ctx = ctx_for(d.path());

        GitBranch
            .call(
                serde_json::json!({ "action": "create", "name": "dup" }),
                &ctx,
            )
            .await
            .expect("the first create should succeed");

        let err = GitBranch
            .call(
                serde_json::json!({ "action": "create", "name": "dup" }),
                &ctx,
            )
            .await
            .expect_err("creating the same branch again should fail");
        assert!(
            matches!(err, ToolError::InvalidArguments { .. }),
            "expected InvalidArguments, got {err:?}"
        );
    }

    // A plain (non-repo) directory makes git.status Unavailable. Requirement 3.3.
    #[tokio::test]
    async fn status_non_repo_is_unavailable() {
        let d = TempDir::new().unwrap();

        let err = GitStatus
            .call(serde_json::json!({}), &ctx_for(d.path()))
            .await
            .expect_err("status outside a repo should fail");
        assert!(
            matches!(err, ToolError::Unavailable { .. }),
            "expected Unavailable, got {err:?}"
        );
    }

    // A push-like action is refused as InvalidArguments with no remote contact. Requirement 3.5.
    #[tokio::test]
    async fn branch_push_action_is_invalid_arguments() {
        let d = init_repo();
        seed_commit(d.path());

        let err = GitBranch
            .call(serde_json::json!({ "action": "push" }), &ctx_for(d.path()))
            .await
            .expect_err("a push action should be refused");
        assert!(
            matches!(err, ToolError::InvalidArguments { .. }),
            "expected InvalidArguments, got {err:?}"
        );
    }
}
