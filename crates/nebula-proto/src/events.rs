//! Daemon-to-client events (the `method` and `params` of a [`crate::Notification`]).

use serde::{Deserialize, Serialize};

use crate::ids::ChatId;
use crate::types::{LogEvent, ModelState, ResourceSnapshot, StopReason, Usage};

/// Every Phase 0 event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Event {
    /// A piece of streamed output.
    #[serde(rename = "chat.token")]
    ChatToken(ChatToken),
    /// A chat finished.
    #[serde(rename = "chat.done")]
    ChatDone(ChatDone),
    /// A chat failed.
    #[serde(rename = "chat.error")]
    ChatError(ChatError),
    /// The model server changed state.
    #[serde(rename = "model.state_changed")]
    ModelStateChanged(ModelStateChanged),
    /// A periodic resource sample.
    #[serde(rename = "resources.snapshot")]
    ResourcesSnapshot(ResourceSnapshot),
    /// A log event, for connections that called `logs.subscribe`.
    #[serde(rename = "log.event")]
    LogEvent(LogEvent),
}

impl Event {
    /// The JSON-RPC method name, e.g. `chat.token`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ChatToken(_) => "chat.token",
            Self::ChatDone(_) => "chat.done",
            Self::ChatError(_) => "chat.error",
            Self::ModelStateChanged(_) => "model.state_changed",
            Self::ResourcesSnapshot(_) => "resources.snapshot",
            Self::LogEvent(_) => "log.event",
        }
    }
}

/// Streamed output. Reasoning and answer text arrive separately so clients can hide reasoning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatToken {
    /// Chat the text belongs to.
    pub chat_id: ChatId,
    /// Answer text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Reasoning text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
}

/// End of a chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatDone {
    /// Finished chat.
    pub chat_id: ChatId,
    /// Why it stopped.
    pub stop_reason: StopReason,
    /// Token counts and timings.
    pub usage: Usage,
}

/// A failed chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatError {
    /// Failed chat.
    pub chat_id: ChatId,
    /// JSON-RPC style error code (see [`crate::error_code`]).
    pub code: i64,
    /// Human-readable message.
    pub message: String,
}

/// A model server state transition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelStateChanged {
    /// Profile the server runs.
    pub profile: String,
    /// Previous state.
    pub from: ModelState,
    /// New state.
    pub to: ModelState,
    /// Why, for failures and restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
