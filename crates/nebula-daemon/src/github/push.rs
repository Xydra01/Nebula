//! Git push engine with child-process isolated authentication and safety guardrails.
//!
//! Pushes the retained task branch as `Nebula-dev-bot`.
//! The token is provided exclusively through `GIT_CONFIG_*` environment variables on the
//! child process, ensuring it never appears on the command line, in git config on disk,
//! or in the remote URL (AGENTS.md hard rule 3).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use thiserror::Error;
use tokio::process::Command;

use super::safety::{PushSafetyError, format_push_refspec, validate_push_ref};

/// Default timeout for git push.
const DEFAULT_PUSH_TIMEOUT: Duration = Duration::from_secs(60);

/// Errors produced during git push.
#[derive(Debug, Error)]
pub enum PushError {
    /// Safety validation refused the push.
    #[error("safety check refused push: {0}")]
    Safety(#[from] PushSafetyError),
    /// Failed to execute or spawn git process.
    #[error("failed to execute git: {0}")]
    Io(#[from] std::io::Error),
    /// Git push timed out.
    #[error("git push timed out after {0:?}")]
    Timeout(Duration),
    /// Git push exited with non-zero exit code.
    #[error("git push failed (exit code {code:?}): {stderr}")]
    Failed {
        /// Process exit code, if available.
        code: Option<i32>,
        /// Scrubbed stderr diagnostics.
        stderr: String,
    },
}

/// RFC 4648 standard base64 encoding without third-party dependencies.
#[must_use]
pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);

    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        let triple = ((u32::from(b0)) << 16) | ((u32::from(b1)) << 8) | u32::from(b2);

        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }

        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

/// Scrub sensitive tokens and basic auth strings from error output before returning/logging.
fn scrub_secret(text: &str, secret: &str) -> String {
    let mut scrubbed = text.to_owned();
    if !secret.is_empty() {
        scrubbed = scrubbed.replace(secret, "[REDACTED_SECRET]");
        let basic = base64_encode(format!("x-access-token:{secret}").as_bytes());
        scrubbed = scrubbed.replace(&basic, "[REDACTED_BASIC_AUTH]");
    }
    scrubbed
}

/// Pushes a task branch to `remote_url` using `token` for authentication.
///
/// # Security Invariants:
/// - Validates that `branch` is safe to push (never `main`, `master`, or base; never force).
/// - Token is passed exclusively to the child process environment via `GIT_CONFIG_*`.
/// - Token is never in process arguments, remote URL, or stored on disk.
/// - Errors are scrubbed of any instance of `token`.
///
/// # Errors
/// Returns [`PushError`] if validation fails or git push fails.
pub async fn push_task_branch(
    repo_root: &Path,
    branch: &str,
    remote_url: &str,
    token: &str,
    base_branch: &str,
    branch_prefix: &str,
) -> Result<(), PushError> {
    // 1. Safety check
    validate_push_ref(branch, base_branch, branch_prefix)?;

    let refspec = format_push_refspec(branch);
    let basic_auth = base64_encode(format!("x-access-token:{token}").as_bytes());

    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root)
        .arg("push")
        .arg(remote_url)
        .arg(&refspec)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
        .env(
            "GIT_CONFIG_VALUE_0",
            format!("AUTHORIZATION: basic {basic_auth}"),
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn()?;
    let result = tokio::time::timeout(DEFAULT_PUSH_TIMEOUT, child.wait_with_output()).await;

    match result {
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let scrubbed = scrub_secret(&stderr, token);
                Err(PushError::Failed {
                    code: output.status.code(),
                    stderr: scrubbed,
                })
            }
        }
        Ok(Err(e)) => Err(PushError::Io(e)),
        Err(_) => Err(PushError::Timeout(DEFAULT_PUSH_TIMEOUT)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");

        let custom = "x-access-token:mock_token_for_test";
        assert_eq!(
            base64_encode(custom.as_bytes()),
            "eC1hY2Nlc3MtdG9rZW46bW9ja190b2tlbl9mb3JfdGVzdA=="
        );
    }

    #[test]
    fn test_scrub_secret() {
        let secret = "mock_secret_token_abc123";
        let raw = format!("fatal: unable to access url with token {secret}");
        let scrubbed = scrub_secret(&raw, secret);
        assert!(!scrubbed.contains(secret));
        assert!(scrubbed.contains("[REDACTED_SECRET]"));

        let basic = base64_encode(format!("x-access-token:{secret}").as_bytes());
        let raw_basic = format!("Authorization: basic {basic}");
        let scrubbed_basic = scrub_secret(&raw_basic, secret);
        assert!(!scrubbed_basic.contains(&basic));
        assert!(scrubbed_basic.contains("[REDACTED_BASIC_AUTH]"));
    }
}
