//! Backend-neutral request and stream types.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::Stream;
use nebula_proto::{ChatMessage, ReasoningEffort, StopReason, TraceId, Usage};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::ModelError;

/// One chat completion request.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatRequest {
    /// Conversation so far.
    pub messages: Vec<ChatMessage>,
    /// OpenAI-style tool definitions, in a fixed order (the prompt cache depends on it).
    pub tools: Vec<Value>,
    /// Constrain the output to this JSON Schema.
    pub response_schema: Option<Value>,
    /// Generation cap.
    pub max_tokens: u32,
    /// Reasoning effort; picks the sampling preset too.
    pub reasoning: ReasoningEffort,
    /// Trace this call belongs to.
    pub trace_id: Option<TraceId>,
}

impl ChatRequest {
    /// A request with no tools or schema, reasoning off and a 1024-token cap.
    #[must_use]
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            response_schema: None,
            max_tokens: 1024,
            reasoning: ReasoningEffort::None,
            trace_id: None,
        }
    }
}

/// A complete tool call, emitted once its streamed fragments are assembled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    /// Call id assigned by the server.
    pub id: String,
    /// Function name.
    pub name: String,
    /// Arguments as a JSON string (not validated here).
    pub arguments: String,
}

/// Items yielded by a [`ChatStream`], ending with exactly one `Done` on success.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamItem {
    /// Generated text. `reasoning` marks reasoning-channel text.
    Token {
        /// Text fragment.
        text: String,
        /// Whether this belongs to the reasoning channel.
        reasoning: bool,
    },
    /// A tool call, after its fragments are complete.
    ToolCall(ToolCall),
    /// Token counts and timings.
    Usage(Usage),
    /// End of the response.
    Done(StopReason),
}

/// Streamed chat output. Dropping it cancels the request on the server.
pub struct ChatStream {
    rx: mpsc::Receiver<Result<StreamItem, ModelError>>,
}

impl ChatStream {
    pub(crate) fn new(rx: mpsc::Receiver<Result<StreamItem, ModelError>>) -> Self {
        Self { rx }
    }

    /// Drains the stream into a [`ChatOutput`].
    ///
    /// # Errors
    /// The first error the stream yields, or [`ModelError::Protocol`] if it ends without `Done`.
    pub async fn collect(mut self) -> Result<ChatOutput, ModelError> {
        let mut out = ChatOutput::default();
        while let Some(item) = self.rx.recv().await {
            match item? {
                StreamItem::Token { text, reasoning } => {
                    if reasoning {
                        out.reasoning.push_str(&text);
                    } else {
                        out.text.push_str(&text);
                    }
                }
                StreamItem::ToolCall(c) => out.tool_calls.push(c),
                StreamItem::Usage(u) => out.usage = Some(u),
                StreamItem::Done(r) => {
                    out.stop_reason = Some(r);
                    return Ok(out);
                }
            }
        }
        Err(ModelError::Protocol(
            "stream ended without a finish reason".into(),
        ))
    }
}

impl Stream for ChatStream {
    type Item = Result<StreamItem, ModelError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// A fully collected chat response.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatOutput {
    /// Answer text.
    pub text: String,
    /// Reasoning text.
    pub reasoning: String,
    /// Tool calls in order.
    pub tool_calls: Vec<ToolCall>,
    /// Usage, if the server reported it.
    pub usage: Option<Usage>,
    /// Why generation stopped.
    pub stop_reason: Option<StopReason>,
}

/// Result of a health probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendHealth {
    /// Serving requests.
    Ok,
    /// Up but still loading the model.
    Loading,
}
