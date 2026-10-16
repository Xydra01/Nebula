//! Safety and branch validation for GitHub operations.
//!
//! Enforces that Nebula never pushes to `main`, `master`, or the configured base branch,
//! never force-pushes, and only pushes branches with the allowed task prefix (AGENTS.md hard rule 1).

use thiserror::Error;

/// Push safety validation error.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PushSafetyError {
    /// Attempted to push an empty branch name.
    #[error("branch name cannot be empty")]
    EmptyBranch,
    /// Attempted to push to a protected branch (e.g. main or master).
    #[error("refusing to push protected branch '{0}' (AGENTS.md hard rule 1)")]
    ProtectedBranch(String),
    /// Attempted to push directly to the target base branch.
    #[error("refusing to push base branch '{0}' (AGENTS.md hard rule 1)")]
    BaseBranch(String),
    /// Force push was requested or specified via refspec prefix.
    #[error("force-push is strictly refused (AGENTS.md hard rule 1)")]
    ForcePushRefused,
    /// Branch does not start with the required prefix.
    #[error("branch '{branch}' does not start with required prefix '{expected_prefix}'")]
    DisallowedPrefix {
        /// Rejected branch.
        branch: String,
        /// Expected prefix.
        expected_prefix: String,
    },
    /// Branch contains illegal git ref characters.
    #[error("invalid characters in branch name '{0}'")]
    InvalidBranchSyntax(String),
}

/// Validates that a branch is safe to push.
///
/// # Invariants enforced:
/// - Not empty
/// - Not `main`, `master`, or matching `base_branch`
/// - Does not start with `+` (force refspec syntax)
/// - Starts with `branch_prefix` (e.g. `nebula/`)
/// - Does not contain illegal ref characters (`..`, `~`, `^`, `:`, `?`, `*`, `[`, `\`, space, control chars)
///
/// # Errors
/// Returns [`PushSafetyError`] if any invariant is violated.
pub fn validate_push_ref(
    branch: &str,
    base_branch: &str,
    branch_prefix: &str,
) -> Result<(), PushSafetyError> {
    let trimmed = branch.trim();
    if trimmed.is_empty() {
        return Err(PushSafetyError::EmptyBranch);
    }

    if trimmed.starts_with('+') {
        return Err(PushSafetyError::ForcePushRefused);
    }

    // Strip leading refs/heads/ if caller passed full ref name
    let clean_branch = trimmed.strip_prefix("refs/heads/").unwrap_or(trimmed);

    // Protected branch check
    if clean_branch.eq_ignore_ascii_case("main") || clean_branch.eq_ignore_ascii_case("master") {
        return Err(PushSafetyError::ProtectedBranch(clean_branch.to_owned()));
    }

    let clean_base = base_branch
        .trim()
        .strip_prefix("refs/heads/")
        .unwrap_or(base_branch.trim());
    if clean_branch.eq_ignore_ascii_case(clean_base) {
        return Err(PushSafetyError::BaseBranch(clean_branch.to_owned()));
    }

    // Prefix check
    if !clean_branch.starts_with(branch_prefix) {
        return Err(PushSafetyError::DisallowedPrefix {
            branch: clean_branch.to_owned(),
            expected_prefix: branch_prefix.to_owned(),
        });
    }

    // Git ref name syntax validation
    // Disallow:
    // - double dots ..
    // - control characters (ASCII < 32 or 127)
    // - space, ~, ^, :, ?, *, [, \
    // - ends with .lock, /, .
    // - contains @{
    if clean_branch.contains("..")
        || clean_branch.contains("@{")
        || clean_branch.ends_with(".lock")
        || clean_branch.ends_with('/')
        || clean_branch.ends_with('.')
        || clean_branch.chars().any(|c| {
            c.is_ascii_control()
                || c == ' '
                || c == '~'
                || c == '^'
                || c == ':'
                || c == '?'
                || c == '*'
                || c == '['
                || c == '\\'
        })
    {
        return Err(PushSafetyError::InvalidBranchSyntax(
            clean_branch.to_owned(),
        ));
    }

    Ok(())
}

/// Builds the explicit, unambiguous refspec for git push: `refs/heads/<branch>:refs/heads/<branch>`.
///
/// Never includes force (`+`) flags.
#[must_use]
pub fn format_push_refspec(branch: &str) -> String {
    let clean = branch
        .trim()
        .strip_prefix('+')
        .unwrap_or(branch.trim())
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.trim());
    format!("refs/heads/{clean}:refs/heads/{clean}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_task_branch() {
        assert!(validate_push_ref("nebula/task-123", "main", "nebula/").is_ok());
        assert!(validate_push_ref("nebula/01ABCDEF", "main", "nebula/").is_ok());
        assert!(validate_push_ref("refs/heads/nebula/task-1", "main", "nebula/").is_ok());
    }

    #[test]
    fn test_refuse_main_and_master() {
        assert_eq!(
            validate_push_ref("main", "develop", "nebula/"),
            Err(PushSafetyError::ProtectedBranch("main".into()))
        );
        assert_eq!(
            validate_push_ref("refs/heads/main", "develop", "nebula/"),
            Err(PushSafetyError::ProtectedBranch("main".into()))
        );
        assert_eq!(
            validate_push_ref("master", "develop", "nebula/"),
            Err(PushSafetyError::ProtectedBranch("master".into()))
        );
    }

    #[test]
    fn test_refuse_base_branch() {
        assert_eq!(
            validate_push_ref("develop", "develop", "nebula/"),
            Err(PushSafetyError::BaseBranch("develop".into()))
        );
        assert_eq!(
            validate_push_ref("refs/heads/staging", "staging", "nebula/"),
            Err(PushSafetyError::BaseBranch("staging".into()))
        );
    }

    #[test]
    fn test_refuse_force_push() {
        assert_eq!(
            validate_push_ref("+nebula/task-1", "main", "nebula/"),
            Err(PushSafetyError::ForcePushRefused)
        );
    }

    #[test]
    fn test_refuse_missing_prefix() {
        assert_eq!(
            validate_push_ref("feat/new-ui", "main", "nebula/"),
            Err(PushSafetyError::DisallowedPrefix {
                branch: "feat/new-ui".into(),
                expected_prefix: "nebula/".into(),
            })
        );
    }

    #[test]
    fn test_refuse_invalid_characters() {
        assert!(matches!(
            validate_push_ref("nebula/foo..bar", "main", "nebula/"),
            Err(PushSafetyError::InvalidBranchSyntax(_))
        ));
        assert!(matches!(
            validate_push_ref("nebula/foo bar", "main", "nebula/"),
            Err(PushSafetyError::InvalidBranchSyntax(_))
        ));
        assert!(matches!(
            validate_push_ref("nebula/foo~1", "main", "nebula/"),
            Err(PushSafetyError::InvalidBranchSyntax(_))
        ));
        assert!(matches!(
            validate_push_ref("nebula/foo^2", "main", "nebula/"),
            Err(PushSafetyError::InvalidBranchSyntax(_))
        ));
        assert!(matches!(
            validate_push_ref("nebula/foo:bar", "main", "nebula/"),
            Err(PushSafetyError::InvalidBranchSyntax(_))
        ));
        assert!(matches!(
            validate_push_ref("nebula/foo.lock", "main", "nebula/"),
            Err(PushSafetyError::InvalidBranchSyntax(_))
        ));
    }

    #[test]
    fn test_format_push_refspec() {
        assert_eq!(
            format_push_refspec("nebula/task-123"),
            "refs/heads/nebula/task-123:refs/heads/nebula/task-123"
        );
        assert_eq!(
            format_push_refspec("refs/heads/nebula/task-123"),
            "refs/heads/nebula/task-123:refs/heads/nebula/task-123"
        );
        assert_eq!(
            format_push_refspec("+refs/heads/nebula/task-123"),
            "refs/heads/nebula/task-123:refs/heads/nebula/task-123"
        );
    }
}
