//! Worktree configuration: [`WorktreeConfig`] and the `[worktree]` config keys.
//!
//! [`WorktreeConfig`] is the typed form of the `[worktree]` section of Nebula's configuration. It
//! is mirrored into `nebula-config`'s `NebulaConfig` as `pub worktree: WorktreeConfig` and its
//! defaults are embedded in `config/default.toml` (the config convention: a new key goes in both).
//! Both fields default so an empty or absent `[worktree]` section parses, and the struct is
//! `#[serde(deny_unknown_fields)]` so a typo under `[worktree]` is a parse error rather than a
//! silent no-op.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::worktree::error::WorktreeError;

/// The placeholder a [`WorktreeConfig::branch_name_format`] must contain; it is replaced with the
/// `Task_Id` when a `Task_Branch` is named (default format `nebula/{task_id}`).
pub const TASK_ID_PLACEHOLDER: &str = "{task_id}";

/// Default parent directory for all per-task worktrees (on the hot drive, per settled decision).
fn default_worktrees_dir() -> PathBuf {
    PathBuf::from(r"F:\Nebula\worktrees")
}

/// Default `Task_Branch` name format. The `nebula/` namespace never collides with the owner's own
/// `feat/*` / `fix/*` branches, and issue #36's bot PR flow can filter on the prefix.
fn default_branch_name_format() -> String {
    "nebula/{task_id}".to_owned()
}

/// The `[worktree]` section (GitHub issue #29).
///
/// Each Nebula task runs in its own git worktree and branch, so concurrent tasks are isolated and
/// the owner's main checkout is never modified. Both fields are resolved from configuration — no
/// drive path is hard-coded (Requirement 8.1).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeConfig {
    /// Parent directory for all per-task worktrees, on the hot drive and never the retired drive
    /// (Requirements 8.1, 8.3). Each task's worktree is created at `<worktrees_dir>\<task-id>`.
    /// Default `F:\Nebula\worktrees`.
    #[serde(default = "default_worktrees_dir")]
    pub worktrees_dir: PathBuf,
    /// Format string for a task's branch name. The `{task_id}` placeholder (see
    /// [`TASK_ID_PLACEHOLDER`]) is replaced with the `Task_Id`. Default `nebula/{task_id}`
    /// (settled decision) — the `nebula/` prefix keeps task branches clear of the owner's own
    /// `feat/*` / `fix/*` branches.
    #[serde(default = "default_branch_name_format")]
    pub branch_name_format: String,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            worktrees_dir: default_worktrees_dir(),
            branch_name_format: default_branch_name_format(),
        }
    }
}

impl WorktreeConfig {
    /// Validate the `[worktree]` section against the configured `retired_drive`.
    ///
    /// Rejects a [`branch_name_format`](Self::branch_name_format) that omits the `{task_id}`
    /// placeholder (every task would otherwise be assigned the same branch name) and a
    /// [`worktrees_dir`](Self::worktrees_dir) that lies on the retired drive (AGENTS.md hard rule
    /// 5; Requirement 8.2). `retired_drive` is the configured `resources.retired_drive` string
    /// (for example `"C:"`); only its leading drive letter is compared, case-insensitively.
    ///
    /// # Errors
    /// [`WorktreeError::InvalidConfig`] when the branch format omits `{task_id}`, or
    /// [`WorktreeError::RetiredDrive`] when `worktrees_dir` is on the retired drive.
    pub fn validate(&self, retired_drive: &str) -> Result<(), WorktreeError> {
        if !self.branch_name_format.contains(TASK_ID_PLACEHOLDER) {
            return Err(WorktreeError::InvalidConfig(format!(
                "worktree.branch_name_format {:?} must contain the {TASK_ID_PLACEHOLDER} placeholder",
                self.branch_name_format
            )));
        }
        if on_retired_drive(&self.worktrees_dir, retired_drive) {
            return Err(WorktreeError::RetiredDrive(self.worktrees_dir.clone()));
        }
        Ok(())
    }
}

/// Whether `path`'s leading drive letter case-insensitively equals that of `retired_drive`.
///
/// Mirrors the drive-letter comparison the `Path_Resolver` uses (`nebula_tools::path`), kept
/// self-contained here because `nebula-sandbox` must not depend on `nebula-tools` (that would be a
/// dependency cycle). A path or a `retired_drive` with no leading drive letter is treated as not
/// on the retired drive.
///
/// Shared with [`WorktreeManager::create`](crate::worktree::manager::WorktreeManager::create),
/// which checks both the configured `worktrees_dir` and the computed target path against the
/// retired drive before any git call (Requirements 1.9, 8.2), so the comparison is defined exactly
/// once.
pub(crate) fn on_retired_drive(path: &Path, retired_drive: &str) -> bool {
    match (drive_letter(path), drive_letter_of_str(retired_drive)) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(&b),
        _ => false,
    }
}

/// Extract the drive letter from a path's prefix component (e.g. `C` from `C:\...` or `\\?\C:\...`).
fn drive_letter(path: &Path) -> Option<char> {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(byte) | Prefix::VerbatimDisk(byte) => Some(byte as char),
            _ => None,
        },
        _ => None,
    }
}

/// Extract a drive letter from a configured string such as `"C:"`, `"c"`, or `"C:\\"`.
fn drive_letter_of_str(value: &str) -> Option<char> {
    value.chars().next().filter(char::is_ascii_alphabetic)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty `[worktree]` section parses to the documented defaults (Requirement 8.1): both
    /// fields are `#[serde(default = ...)]`, so an absent key falls back rather than failing.
    #[test]
    fn empty_section_deserializes_to_defaults() {
        let cfg: WorktreeConfig = toml::from_str("").expect("empty [worktree] section parses");
        assert_eq!(cfg, WorktreeConfig::default());
        assert_eq!(cfg.worktrees_dir, PathBuf::from(r"F:\Nebula\worktrees"));
        assert_eq!(cfg.branch_name_format, "nebula/{task_id}");
    }

    /// A supplied field overrides its default while the other field still falls back (Req 8.1).
    #[test]
    fn partial_section_fills_missing_fields_from_defaults() {
        // Single-quoted TOML literal strings do not process escapes, so each backslash survives
        // verbatim into the parsed path.
        let cfg: WorktreeConfig = toml::from_str(r"worktrees_dir = 'F:\alt\worktrees'")
            .expect("partial [worktree] section parses");
        assert_eq!(cfg.worktrees_dir, PathBuf::from(r"F:\alt\worktrees"));
        assert_eq!(cfg.branch_name_format, "nebula/{task_id}");
    }

    /// An unknown key under `[worktree]` is a parse error, not a silent no-op
    /// (`#[serde(deny_unknown_fields)]`; config convention, Req 8.1).
    #[test]
    fn unknown_key_is_rejected() {
        let err = toml::from_str::<WorktreeConfig>("bogus_key = true")
            .expect_err("an unknown key must be rejected");
        assert!(
            err.to_string().contains("bogus_key") || err.to_string().contains("unknown"),
            "{err}"
        );
    }

    /// `validate` rejects a `branch_name_format` with no `{task_id}` placeholder — otherwise every
    /// task would be assigned the same branch name (Req 8.1).
    #[test]
    fn validate_rejects_branch_format_missing_placeholder() {
        let cfg = WorktreeConfig {
            worktrees_dir: PathBuf::from(r"F:\Nebula\worktrees"),
            branch_name_format: "nebula/task".to_owned(),
        };
        let err = cfg
            .validate("C:")
            .expect_err("a format missing {task_id} must fail validate");
        assert!(
            matches!(&err, WorktreeError::InvalidConfig(m) if m.contains(TASK_ID_PLACEHOLDER)),
            "{err}"
        );
    }

    /// `validate` rejects a `worktrees_dir` on the retired drive (AGENTS.md hard rule 5;
    /// Req 8.2). The comparison is case-insensitive on the leading drive letter.
    #[test]
    fn validate_rejects_worktrees_dir_on_retired_drive() {
        let cfg = WorktreeConfig {
            worktrees_dir: PathBuf::from(r"C:\something\worktrees"),
            branch_name_format: "nebula/{task_id}".to_owned(),
        };
        let err = cfg
            .validate("C:")
            .expect_err("a worktrees_dir on the retired drive must fail validate");
        assert!(
            matches!(&err, WorktreeError::RetiredDrive(p) if p == &cfg.worktrees_dir),
            "{err}"
        );
        // Lowercase drive letter in the configured path is matched case-insensitively.
        let lower = WorktreeConfig {
            worktrees_dir: PathBuf::from(r"c:\something"),
            branch_name_format: "nebula/{task_id}".to_owned(),
        };
        assert!(matches!(
            lower.validate("C:"),
            Err(WorktreeError::RetiredDrive(_))
        ));
    }

    /// A fully valid config with a non-retired `worktrees_dir` and a well-formed branch format
    /// passes `validate` (Req 8.1, 8.2).
    #[test]
    fn validate_accepts_a_well_formed_config() {
        let cfg = WorktreeConfig::default();
        cfg.validate("C:").expect("the defaults are valid off C:");
    }
}
