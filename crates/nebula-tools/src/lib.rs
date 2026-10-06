//! The Nebula MCP tool host.
//!
//! Every tool, built-in or external, is called through this one boundary (ADR-002). Phase 1
//! implements the external half: stdio [MCP](https://modelcontextprotocol.io) servers launched
//! from a manifest.
//!
//! - [`launcher::StdioLauncher`] spawns a tool server as a child process inside a Windows Job
//!   Object, so the server dies if Nebula dies, and pipes its stdin/stdout for JSON-RPC.
//! - [`client::McpClient`] speaks newline-delimited JSON-RPC 2.0 over those pipes: the
//!   `initialize` handshake, `tools/list`, and `tools/call`.
//! - [`host::ToolHost`] owns one client per configured server, caches each tool's input schema,
//!   validates call arguments against that schema before the call, enforces a per-call timeout
//!   and an output-size cap, and logs a `tool.call` event with the trace id for every call.
//!
//! The host is deliberately small and hand-rolled rather than built on a full MCP SDK: the only
//! operations Phase 1 needs are the three above, and the launcher must use Nebula's own Job
//! Object so a crashed daemon never leaks tool processes.

pub mod builtins;
pub mod cap;
pub mod client;
pub mod config;
pub mod host;
pub mod launcher;
pub mod path;
pub mod permit;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod types;

pub use builtins::{
    BuiltinLimits, BuiltinTool, ResourceProvider, ToolContext, ToolOutput, WorktreeRootProvider,
    register_all,
};
pub use cap::{TRUNCATION_MARKER, enforce_cap};
pub use client::McpClient;
pub use config::{BuiltinToolsConfig, ToolHostConfig, ToolServerConfig};
pub use host::ToolHost;
pub use launcher::{ChildProcess, LaunchSpec, StdioLauncher, ToolLauncher};
pub use permit::{
    Approval, CommandClassifier, DefaultClassifier, NO_APPROVAL_THRESHOLD, Tier, effective_tier,
};
pub use types::{ToolCallResult, ToolDescriptor};

/// How long a tool server has to answer the `initialize` handshake and `tools/list`.
pub(crate) const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 10_000;
/// Default per-call timeout when a server config does not set one.
pub(crate) const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;
/// Default cap on a single tool result's serialized size.
pub(crate) const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// Tool host failures.
///
/// `Clone`/`Eq` so callers (and tests) can match and compare, matching `nebula_model::ModelError`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// No tool server is configured under that name.
    #[error("unknown tool server {0:?}")]
    UnknownServer(String),
    /// No server exposes a tool with that name.
    #[error("unknown tool {0:?}")]
    UnknownTool(String),
    /// The arguments did not match the tool's input schema; never sent to the server.
    #[error("invalid arguments for {tool:?}: {detail}")]
    InvalidArguments {
        /// Tool whose schema rejected the arguments.
        tool: String,
        /// First validation error.
        detail: String,
    },
    /// The server process could not be started.
    #[error("launch {server:?}: {detail}")]
    Launch {
        /// Server that failed to start.
        server: String,
        /// Underlying reason.
        detail: String,
    },
    /// The server exited, or never finished the handshake.
    #[error("tool server {server:?} unavailable: {detail}")]
    Unavailable {
        /// Server that is not usable.
        server: String,
        /// Underlying reason.
        detail: String,
    },
    /// The call did not finish before its timeout; the server was killed.
    #[error("tool {tool:?} timed out after {timeout_ms} ms")]
    Timeout {
        /// Tool that was being called.
        tool: String,
        /// Deadline that elapsed.
        timeout_ms: u64,
    },
    /// The result exceeded the output-size cap and the full output could not be preserved.
    ///
    /// The cap is normally enforced by truncating the inline output to [`cap`](Self::OutputTooLarge::cap)
    /// bytes and storing the full output in a blob. This error is returned only when that blob
    /// write fails: `prefix` carries the UTF-8-safe truncated inline output for diagnostics, and
    /// no blob reference is recorded.
    #[error(
        "tool {tool:?} returned {got} bytes, over the {cap}-byte cap, and the full output could not be stored"
    )]
    OutputTooLarge {
        /// Tool that produced the output.
        tool: String,
        /// Serialized result size.
        got: usize,
        /// Configured cap.
        cap: usize,
        /// The UTF-8-safe truncated inline output (prefix), kept for the blob-write-failure
        /// diagnostic.
        prefix: String,
    },
    /// A tool with that name is already registered, across both the built-in and external maps.
    #[error("duplicate tool {name:?}: a tool with that name is already registered")]
    DuplicateTool {
        /// The conflicting tool name that is already held by a built-in or external tool.
        name: String,
    },
    /// The server broke the JSON-RPC / MCP contract, or returned an error result.
    #[error("tool server {server:?} protocol error: {detail}")]
    Protocol {
        /// Server that misbehaved.
        server: String,
        /// What was wrong.
        detail: String,
    },
}
