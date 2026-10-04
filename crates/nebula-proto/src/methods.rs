//! Client-to-daemon methods (the `method` and `params` of a [`crate::Request`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{ChatId, TraceId};
use crate::types::{ChatMessage, LogLevel, ModelStatus, ReasoningEffort};

/// Parameters for methods that take none. Serialized as `{}`; JSON-RPC callers must still send
/// `"params": {}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

/// Every Phase 0 method.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Method {
    /// Daemon version, uptime and model state. Result: [`DaemonStatus`].
    #[serde(rename = "daemon.status")]
    DaemonStatus(Empty),
    /// Graceful shutdown: stop accepting clients, cancel chats, stop the model, flush logs.
    /// Result: [`Empty`].
    #[serde(rename = "daemon.shutdown")]
    DaemonShutdown(Empty),
    /// Start a streaming chat. Result: [`ChatStarted`]; tokens follow as `chat.token` events.
    #[serde(rename = "chat.start")]
    ChatStart(ChatStartParams),
    /// Cancel a running chat. Result: [`Empty`].
    #[serde(rename = "chat.cancel")]
    ChatCancel(ChatCancelParams),
    /// Result: [`ModelStatus`].
    #[serde(rename = "model.status")]
    ModelStatus(Empty),
    /// Switch the model profile (drains in-flight requests first). Result: [`ModelStatus`].
    #[serde(rename = "model.set_profile")]
    ModelSetProfile(ModelSetProfileParams),
    /// Result: [`crate::ResourceSnapshot`].
    #[serde(rename = "resources.snapshot")]
    ResourcesSnapshot(Empty),
    /// Stream log events to this connection as `log.event`. Result: [`Empty`].
    #[serde(rename = "logs.subscribe")]
    LogsSubscribe(LogsSubscribeParams),
    /// Result: [`crate::DoctorReport`].
    #[serde(rename = "doctor.run")]
    DoctorRun(Empty),
}

impl Method {
    /// Every method name, in declaration order.
    pub const NAMES: [&'static str; 9] = [
        "daemon.status",
        "daemon.shutdown",
        "chat.start",
        "chat.cancel",
        "model.status",
        "model.set_profile",
        "resources.snapshot",
        "logs.subscribe",
        "doctor.run",
    ];

    /// The JSON-RPC method name, e.g. `chat.start`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::DaemonStatus(_) => "daemon.status",
            Self::DaemonShutdown(_) => "daemon.shutdown",
            Self::ChatStart(_) => "chat.start",
            Self::ChatCancel(_) => "chat.cancel",
            Self::ModelStatus(_) => "model.status",
            Self::ModelSetProfile(_) => "model.set_profile",
            Self::ResourcesSnapshot(_) => "resources.snapshot",
            Self::LogsSubscribe(_) => "logs.subscribe",
            Self::DoctorRun(_) => "doctor.run",
        }
    }
}

/// Parameters of `chat.start`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatStartParams {
    /// Conversation so far, ending with the user's message.
    pub messages: Vec<ChatMessage>,
    /// Profile to use; the active one if absent. A different profile triggers a switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// JSON Schema the reply must match (llama-server `json_schema` response format).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
    /// Generation limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Reasoning effort; `none` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningEffort>,
}

/// Result of `chat.start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatStarted {
    /// Identifies the chat in later events and in `chat.cancel`.
    pub chat_id: ChatId,
    /// Trace covering the chat's log events and model call.
    pub trace_id: TraceId,
}

/// Parameters of `chat.cancel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCancelParams {
    /// Chat to cancel.
    pub chat_id: ChatId,
}

/// Parameters of `model.set_profile`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSetProfileParams {
    /// Profile name from the config.
    pub profile: String,
}

/// Parameters of `logs.subscribe`. All filters are optional and combine with AND.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogsSubscribeParams {
    /// Only events at this level or above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_level: Option<LogLevel>,
    /// Only events in this trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<TraceId>,
    /// Only events whose target starts with this prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_prefix: Option<String>,
}

/// Result of `daemon.status`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonStatus {
    /// Daemon version (crate version).
    pub version: String,
    /// Protocol version the daemon speaks.
    pub proto_version: u32,
    /// Daemon process ID.
    pub pid: u32,
    /// Seconds since start.
    pub uptime_s: u64,
    /// Model server state.
    pub model: ModelStatus,
}
