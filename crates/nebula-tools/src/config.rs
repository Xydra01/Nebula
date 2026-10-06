//! The tool-server manifest (`[tools]` in the daemon config).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::{BuiltinLimits, DEFAULT_CALL_TIMEOUT_MS, DEFAULT_MAX_OUTPUT_BYTES};

/// Smallest permitted built-in call timeout, in seconds (design 1.6).
pub const BUILTIN_CALL_TIMEOUT_MIN_SECS: u64 = 1;
/// Largest permitted built-in call timeout, in seconds (design 1.6).
pub const BUILTIN_CALL_TIMEOUT_MAX_SECS: u64 = 600;
/// Default built-in call timeout, in seconds.
pub const BUILTIN_CALL_TIMEOUT_DEFAULT_SECS: u64 = 30;
/// Default inline output cap for built-ins, in bytes.
pub const BUILTIN_OUTPUT_CAP_DEFAULT_BYTES: usize = 65536;

/// The `[tools]` section: a set of named stdio MCP servers plus the built-in tools.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolHostConfig {
    /// Servers to launch, keyed by a short local name used in logs and tool routing.
    #[serde(default)]
    pub servers: BTreeMap<String, ToolServerConfig>,
    /// Built-in (in-process) tool settings, from `[tools.builtin]`.
    #[serde(default)]
    pub builtin: BuiltinToolsConfig,
}

/// The `[tools.builtin]` section: settings for the in-process built-in tools.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinToolsConfig {
    /// Confinement root for file and shell tools. Every path a built-in touches must resolve
    /// inside this directory. Defaults to the current directory (`"."`).
    #[serde(default = "default_worktree_root")]
    pub worktree_root: PathBuf,
    /// Per-call timeout in seconds. Clamped to `1..=600` when building [`BuiltinLimits`].
    #[serde(default = "default_builtin_call_timeout_secs")]
    pub call_timeout_secs: u64,
    /// Largest inline output in bytes; output beyond this is truncated and preserved in a blob.
    #[serde(default = "default_builtin_output_cap_bytes")]
    pub output_cap_bytes: usize,
}

impl Default for BuiltinToolsConfig {
    fn default() -> Self {
        Self {
            worktree_root: default_worktree_root(),
            call_timeout_secs: default_builtin_call_timeout_secs(),
            output_cap_bytes: default_builtin_output_cap_bytes(),
        }
    }
}

fn default_worktree_root() -> PathBuf {
    PathBuf::from(".")
}

const fn default_builtin_call_timeout_secs() -> u64 {
    BUILTIN_CALL_TIMEOUT_DEFAULT_SECS
}

const fn default_builtin_output_cap_bytes() -> usize {
    BUILTIN_OUTPUT_CAP_DEFAULT_BYTES
}

impl BuiltinToolsConfig {
    /// Builds [`BuiltinLimits`], clamping `call_timeout_secs` to `1..=600` seconds (design 1.6).
    #[must_use]
    pub fn limits(&self) -> BuiltinLimits {
        let secs = self
            .call_timeout_secs
            .clamp(BUILTIN_CALL_TIMEOUT_MIN_SECS, BUILTIN_CALL_TIMEOUT_MAX_SECS);
        BuiltinLimits {
            call_timeout: Duration::from_secs(secs),
            output_cap: self.output_cap_bytes,
        }
    }
}

/// One stdio MCP server in the manifest.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolServerConfig {
    /// Executable to run.
    pub command: PathBuf,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables, as `[[tools.servers.<name>.env]]` entries.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Per-call timeout in milliseconds. A call that runs longer is killed.
    #[serde(default = "default_call_timeout_ms")]
    pub call_timeout_ms: u64,
    /// Largest serialized result accepted from one call, in bytes.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

const fn default_call_timeout_ms() -> u64 {
    DEFAULT_CALL_TIMEOUT_MS
}

const fn default_max_output_bytes() -> usize {
    DEFAULT_MAX_OUTPUT_BYTES
}

impl ToolHostConfig {
    /// Every tool-server command path, labelled, for drive checks.
    #[must_use]
    pub fn command_paths(&self) -> Vec<(String, PathBuf)> {
        self.servers
            .iter()
            .map(|(name, s)| (format!("tools.servers.{name}.command"), s.command.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_defaults_match_the_documented_values() {
        let cfg = BuiltinToolsConfig::default();
        assert_eq!(cfg.worktree_root, PathBuf::from("."));
        assert_eq!(cfg.call_timeout_secs, 30);
        assert_eq!(cfg.output_cap_bytes, 65536);
    }

    #[test]
    fn empty_builtin_table_falls_back_to_defaults() {
        let host: ToolHostConfig = toml::from_str("[builtin]\n").unwrap();
        assert_eq!(host.builtin, BuiltinToolsConfig::default());
    }

    #[test]
    fn full_builtin_table_parses_every_field() {
        let host: ToolHostConfig = toml::from_str(
            r#"
            [builtin]
            worktree_root = 'F:\Nebula\worktrees'
            call_timeout_secs = 45
            output_cap_bytes = 131072
            "#,
        )
        .unwrap();
        assert_eq!(
            host.builtin.worktree_root,
            PathBuf::from(r"F:\Nebula\worktrees")
        );
        assert_eq!(host.builtin.call_timeout_secs, 45);
        assert_eq!(host.builtin.output_cap_bytes, 131072);
    }

    #[test]
    fn limits_clamp_the_call_timeout_to_the_allowed_range() {
        let too_small = BuiltinToolsConfig {
            call_timeout_secs: 0,
            ..BuiltinToolsConfig::default()
        };
        assert_eq!(too_small.limits().call_timeout, Duration::from_secs(1));

        let too_large = BuiltinToolsConfig {
            call_timeout_secs: 10_000,
            ..BuiltinToolsConfig::default()
        };
        assert_eq!(too_large.limits().call_timeout, Duration::from_secs(600));

        let in_range = BuiltinToolsConfig {
            call_timeout_secs: 45,
            ..BuiltinToolsConfig::default()
        };
        assert_eq!(in_range.limits().call_timeout, Duration::from_secs(45));
        assert_eq!(in_range.limits().output_cap, in_range.output_cap_bytes);
    }

    #[test]
    fn unknown_builtin_key_is_rejected() {
        let err = toml::from_str::<BuiltinToolsConfig>("worktree_root = \".\"\nbogus_key = 1\n")
            .unwrap_err();
        assert!(err.to_string().contains("bogus_key"), "{err}");
    }
}
