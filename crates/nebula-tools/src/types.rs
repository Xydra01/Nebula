//! Shared tool-host types: tool descriptors and call results.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool as advertised by a server's `tools/list`, with the server it belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// The server that exposes this tool (the manifest key, not the MCP server name).
    pub server: String,
    /// Tool name, unique across all servers (see [`crate::host::ToolHost`]).
    pub name: String,
    /// Human-readable description, if the server gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the tool's arguments (MCP `inputSchema`). Defaults to an empty object
    /// schema when a server omits it.
    pub input_schema: Value,
}

/// The outcome of a `tools/call`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// Content blocks the tool returned (MCP `content`), passed through unchanged.
    pub content: Value,
    /// Whether the tool itself reported an error (MCP `isError`). A `true` here is a tool-level
    /// failure, distinct from a transport or protocol [`crate::ToolError`].
    #[serde(default)]
    pub is_error: bool,
}
