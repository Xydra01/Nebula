//! The tool-server manifest (`[tools]` in the daemon config).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::{DEFAULT_CALL_TIMEOUT_MS, DEFAULT_MAX_OUTPUT_BYTES};

/// The `[tools]` section: a set of named stdio MCP servers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolHostConfig {
    /// Servers to launch, keyed by a short local name used in logs and tool routing.
    #[serde(default)]
    pub servers: BTreeMap<String, ToolServerConfig>,
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
