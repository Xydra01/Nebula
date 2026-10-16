//! GitHub REST API client for Pull Requests.
//!
//! Provides functions to find existing open PRs, create new PRs, and post follow-up comments.
//! Supports configuring the base URL to allow testing with local mock HTTP servers.

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// GitHub API errors.
#[derive(Debug, Error)]
pub enum GithubApiError {
    /// HTTP request failed.
    #[error("http request failed: {0}")]
    Http(String),
    /// GitHub API returned non-success HTTP status.
    #[error("github api error (status {status}): {message}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Sanitized error message from API response.
        message: String,
    },
    /// JSON parsing failed.
    #[error("failed to deserialize response: {0}")]
    Json(String),
}

/// Head branch reference info from GitHub API.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PullRequestHead {
    /// Git branch ref.
    #[serde(default)]
    pub r#ref: String,
}

/// Representation of a GitHub Pull Request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PullRequest {
    /// PR number.
    pub number: u64,
    /// Public web URL.
    pub html_url: String,
    /// PR title.
    pub title: String,
    /// PR body markdown.
    #[serde(default)]
    pub body: Option<String>,
    /// PR state ("open", "closed").
    pub state: String,
    /// Head branch info, if provided.
    #[serde(default)]
    pub head: Option<PullRequestHead>,
}

#[derive(Serialize)]
struct CreatePrPayload<'a> {
    title: &'a str,
    body: &'a str,
    head: &'a str,
    base: &'a str,
}

#[derive(Serialize)]
struct CreateCommentPayload<'a> {
    body: &'a str,
}

/// GitHub REST API client.
#[derive(Clone, Debug)]
pub struct GithubClient {
    client: reqwest::Client,
    base_url: String,
    token: String,
    bot_username: String,
}

impl GithubClient {
    /// Creates a new GitHub client using the standard GitHub API URL.
    ///
    /// # Errors
    /// Returns [`GithubApiError`] if HTTP client construction fails.
    pub fn new(token: &str, bot_username: &str) -> Result<Self, GithubApiError> {
        Self::with_base_url(token, bot_username, "https://api.github.com")
    }

    /// Creates a new GitHub client targeting a custom base URL (used in tests).
    ///
    /// # Errors
    /// Returns [`GithubApiError`] if HTTP client construction fails.
    pub fn with_base_url(
        token: &str,
        bot_username: &str,
        base_url: &str,
    ) -> Result<Self, GithubApiError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| GithubApiError::Http(e.to_string()))?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            bot_username: bot_username.to_owned(),
        })
    }

    fn headers(&self) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Ok(mut val) = HeaderValue::from_str(&format!("Bearer {}", self.token)) {
            val.set_sensitive(true);
            map.insert(AUTHORIZATION, val);
        }
        if let Ok(val) = HeaderValue::from_str(&self.bot_username) {
            map.insert(USER_AGENT, val);
        }
        map.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        map.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        map
    }

    fn sanitize_err(&self, err_text: &str) -> String {
        if !self.token.is_empty() {
            err_text.replace(&self.token, "[REDACTED]")
        } else {
            err_text.to_owned()
        }
    }

    /// Finds an existing open PR for `head_branch`.
    ///
    /// # Errors
    /// Returns [`GithubApiError`] if network or API call fails.
    pub async fn find_open_pr(
        &self,
        repo: &str,
        head_branch: &str,
    ) -> Result<Option<PullRequest>, GithubApiError> {
        let clean_head = head_branch
            .strip_prefix("refs/heads/")
            .unwrap_or(head_branch);
        let url = format!("{}/repos/{}/pulls?state=open", self.base_url, repo);

        let resp = self
            .client
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(|e| GithubApiError::Http(self.sanitize_err(&e.to_string())))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(GithubApiError::Status {
                status,
                message: self.sanitize_err(&text),
            });
        }

        let prs: Vec<PullRequest> = resp
            .json()
            .await
            .map_err(|e| GithubApiError::Json(self.sanitize_err(&e.to_string())))?;

        let found = prs.into_iter().find(|pr| {
            if let Some(h) = &pr.head {
                h.r#ref == clean_head
            } else {
                pr.html_url.ends_with(&format!("/pull/{}", pr.number))
            }
        });

        Ok(found)
    }

    /// Opens a new Pull Request.
    ///
    /// # Errors
    /// Returns [`GithubApiError`] if the call fails.
    pub async fn create_pr(
        &self,
        repo: &str,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, GithubApiError> {
        let clean_head = head.strip_prefix("refs/heads/").unwrap_or(head);
        let clean_base = base.strip_prefix("refs/heads/").unwrap_or(base);
        let url = format!("{}/repos/{}/pulls", self.base_url, repo);

        let payload = CreatePrPayload {
            title,
            body,
            head: clean_head,
            base: clean_base,
        };

        let resp = self
            .client
            .post(&url)
            .headers(self.headers())
            .json(&payload)
            .send()
            .await
            .map_err(|e| GithubApiError::Http(self.sanitize_err(&e.to_string())))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(GithubApiError::Status {
                status,
                message: self.sanitize_err(&text),
            });
        }

        let pr: PullRequest = resp
            .json()
            .await
            .map_err(|e| GithubApiError::Json(self.sanitize_err(&e.to_string())))?;

        Ok(pr)
    }

    /// Adds a comment to an existing PR (or issue).
    ///
    /// # Errors
    /// Returns [`GithubApiError`] if comment creation fails.
    pub async fn add_pr_comment(
        &self,
        repo: &str,
        pr_number: u64,
        comment: &str,
    ) -> Result<(), GithubApiError> {
        let url = format!(
            "{}/repos/{}/issues/{}/comments",
            self.base_url, repo, pr_number
        );
        let payload = CreateCommentPayload { body: comment };

        let resp = self
            .client
            .post(&url)
            .headers(self.headers())
            .json(&payload)
            .send()
            .await
            .map_err(|e| GithubApiError::Http(self.sanitize_err(&e.to_string())))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(GithubApiError::Status {
                status,
                message: self.sanitize_err(&text),
            });
        }

        Ok(())
    }
}
