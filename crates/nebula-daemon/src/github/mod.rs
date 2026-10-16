//! GitHub flow: automated branch push and PR creation as `Nebula-dev-bot` (issue #36).
//!
//! Provides safe branch pushing, token isolation, refspec validation, and PR creation.

pub mod api;
pub mod format;
pub mod push;
pub mod safety;

use std::path::Path;

use nebula_config::GitHubConfig;
use nebula_proto::TraceId;
use thiserror::Error;

pub use api::{GithubApiError, GithubClient, PullRequest};
pub use format::{format_pr_body, format_pr_title};
pub use push::{PushError, push_task_branch};
pub use safety::{PushSafetyError, format_push_refspec, validate_push_ref};

/// GitHub flow errors.
#[derive(Debug, Error)]
pub enum GithubFlowError {
    /// Safety check failed.
    #[error("safety check failed: {0}")]
    Safety(#[from] PushSafetyError),
    /// Git push failed.
    #[error("git push failed: {0}")]
    Push(#[from] PushError),
    /// GitHub API failed.
    #[error("github api failed: {0}")]
    Api(#[from] GithubApiError),
    /// Bot token was missing.
    #[error("bot token not found in Credential Manager target '{0}'")]
    MissingToken(String),
}

/// Parameters for handling task completion GitHub flow.
pub struct TaskCompletionParams<'a> {
    /// Worktree or repo root where git commands will run.
    pub repo_root: &'a Path,
    /// The retained task branch name (e.g. `nebula/01...`).
    pub branch: &'a str,
    /// Task goal.
    pub goal: &'a str,
    /// Task final summary.
    pub summary: &'a str,
    /// Trace ID.
    pub trace_id: TraceId,
    /// Verification and test results output.
    pub test_results: Option<&'a str>,
    /// GitHub configuration.
    pub config: &'a GitHubConfig,
    /// Optional token override for testing.
    pub token_override: Option<&'a str>,
    /// Optional base URL override for testing.
    pub api_base_url_override: Option<&'a str>,
    /// Optional git remote URL override for testing.
    pub git_remote_url_override: Option<&'a str>,
    /// Optional extra co-author trailer.
    pub extra_coauthor: Option<&'a str>,
}

/// Executes the full GitHub flow upon task completion:
/// 1. Reads the bot token from Windows Credential Manager (or override).
/// 2. Validates branch safety (refusing `main`, `master`, base, force-push).
/// 3. Pushes the branch using child-process-isolated `GIT_CONFIG_*` extraheader basic auth.
/// 4. Opens or updates the GitHub Pull Request with description, trace ID, test results, and attribution.
///
/// Returns `Ok(Some(pr_url))` if PR was opened, or `Ok(None)` if flow was skipped (e.g. disabled or token missing).
///
/// # Errors
/// Returns [`GithubFlowError`] if push or API operations fail.
pub async fn handle_task_completion(
    params: TaskCompletionParams<'_>,
) -> Result<Option<String>, GithubFlowError> {
    // 1. Obtain token
    let token = if let Some(t) = params.token_override {
        t.to_owned()
    } else {
        #[cfg(windows)]
        {
            match crate::win::read_credential(&params.config.token_target) {
                Some(t) if !t.is_empty() => t,
                _ => {
                    tracing::warn!(
                        event = "github.token_missing",
                        target = %params.config.token_target,
                        "bot token not found in Credential Manager; skipping PR"
                    );
                    return Ok(None);
                }
            }
        }
        #[cfg(not(windows))]
        {
            tracing::warn!(
                event = "github.token_missing",
                target = %params.config.token_target,
                "Credential Manager only supported on Windows; skipping PR"
            );
            return Ok(None);
        }
    };

    // 2. Push task branch
    let remote_url = params
        .git_remote_url_override
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("https://github.com/{}.git", params.config.repo));
    tracing::info!(
        event = "github.pushing_branch",
        branch = %params.branch,
        repo = %params.config.repo
    );

    push_task_branch(
        params.repo_root,
        params.branch,
        &remote_url,
        &token,
        &params.config.base_branch,
        &params.config.branch_prefix,
    )
    .await?;

    tracing::info!(
        event = "github.branch_pushed",
        branch = %params.branch,
        repo = %params.config.repo
    );

    // 3. Format PR content
    let title = format_pr_title(params.goal);
    let body = format_pr_body(
        params.goal,
        params.summary,
        params.trace_id,
        params.test_results,
        &params.config.bot_username,
        params.extra_coauthor,
    );

    // 4. Create or update PR
    let client = if let Some(base_url) = params.api_base_url_override {
        GithubClient::with_base_url(&token, &params.config.bot_username, base_url)?
    } else {
        GithubClient::new(&token, &params.config.bot_username)?
    };

    tracing::info!(
        event = "github.opening_pr",
        repo = %params.config.repo,
        branch = %params.branch,
        base = %params.config.base_branch
    );

    let pr_result = client
        .create_pr(
            &params.config.repo,
            &params.config.base_branch,
            params.branch,
            &title,
            &body,
        )
        .await;

    let pr = match pr_result {
        Ok(pr) => pr,
        Err(GithubApiError::Status { status: 422, .. }) => {
            // PR already exists (e.g. follow-up commits after review)
            tracing::info!(
                event = "github.pr_already_exists",
                branch = %params.branch,
                "PR already exists for branch; searching for existing PR"
            );
            if let Ok(Some(existing)) = client
                .find_open_pr(&params.config.repo, params.branch)
                .await
            {
                let comment = format!(
                    "Follow-up task completed.\n\n## Trace ID\n`{}`\n\n## Summary\n{}",
                    params.trace_id, params.summary
                );
                let _ = client
                    .add_pr_comment(&params.config.repo, existing.number, &comment)
                    .await;
                existing
            } else {
                return Err(GithubFlowError::Api(GithubApiError::Status {
                    status: 422,
                    message: "A pull request already exists for this branch".to_owned(),
                }));
            }
        }
        Err(e) => return Err(GithubFlowError::Api(e)),
    };

    tracing::info!(
        event = "github.pr_opened",
        pr_number = pr.number,
        pr_url = %pr.html_url
    );

    Ok(Some(pr.html_url))
}
