//! Parser for llama-server's OpenAI-compatible streaming chunks.

use std::collections::BTreeMap;

use nebula_proto::{StopReason, Usage};
use serde::Deserialize;

use crate::ModelError;
use crate::types::{StreamItem, ToolCall};

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    timings: Option<Timings>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct Timings {
    #[serde(default)]
    prompt_n: f64,
    #[serde(default)]
    prompt_ms: f64,
    #[serde(default)]
    predicted_n: f64,
    #[serde(default)]
    predicted_ms: f64,
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "token counts are small non-negative integers sent as JSON numbers"
)]
fn count(n: f64) -> u32 {
    n.max(0.0).round() as u32
}

/// Turns SSE `data:` payloads into [`StreamItem`]s, assembling tool-call fragments.
///
/// `Done` is held back until `[DONE]` (or the end of the stream), so the usage that arrives
/// on the final chunk is always yielded before it.
#[derive(Debug, Default)]
pub struct ChunkParser {
    tool_calls: BTreeMap<u32, ToolCall>,
    finish: Option<StopReason>,
    usage_sent: bool,
    done: bool,
}

impl ChunkParser {
    /// A fresh parser for one response.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `Done` has been produced.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Feeds one `data:` payload.
    ///
    /// # Errors
    /// [`ModelError::Protocol`] for JSON that isn't a chunk, or an `error` object.
    pub fn feed(&mut self, data: &str) -> Result<Vec<StreamItem>, ModelError> {
        let data = data.trim();
        if data == "[DONE]" {
            return Ok(self.finish());
        }
        let value: serde_json::Value = serde_json::from_str(data)
            .map_err(|e| ModelError::Protocol(format!("bad chunk: {e}")))?;
        if let Some(err) = value.get("error") {
            return Err(ModelError::Protocol(format!("server error: {err}")));
        }
        let chunk: Chunk = serde_json::from_value(value)
            .map_err(|e| ModelError::Protocol(format!("bad chunk: {e}")))?;
        let mut out = Vec::new();
        for choice in chunk.choices {
            if let Some(text) = choice.delta.reasoning_content.filter(|t| !t.is_empty()) {
                out.push(StreamItem::Token {
                    text,
                    reasoning: true,
                });
            }
            if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
                out.push(StreamItem::Token {
                    text,
                    reasoning: false,
                });
            }
            for d in choice.delta.tool_calls {
                let call = self.tool_calls.entry(d.index).or_insert_with(|| ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                if let Some(id) = d.id {
                    call.id = id;
                }
                if let Some(f) = d.function {
                    if let Some(n) = f.name {
                        call.name.push_str(&n);
                    }
                    if let Some(a) = f.arguments {
                        call.arguments.push_str(&a);
                    }
                }
            }
            if let Some(reason) = choice.finish_reason {
                self.finish = Some(match reason.as_str() {
                    "length" => StopReason::Length,
                    "tool_calls" => StopReason::ToolCalls,
                    _ => StopReason::Stop,
                });
            }
        }
        if let Some(t) = chunk.timings
            && self.finish.is_some()
            && !self.usage_sent
        {
            self.usage_sent = true;
            out.push(StreamItem::Usage(Usage {
                prompt_n: count(t.prompt_n),
                prompt_ms: t.prompt_ms,
                predicted_n: count(t.predicted_n),
                predicted_ms: t.predicted_ms,
            }));
        }
        Ok(out)
    }

    /// Ends the response: flushes assembled tool calls and `Done`. Returns nothing if the
    /// server never sent a finish reason (the caller reports that as an error).
    pub fn finish(&mut self) -> Vec<StreamItem> {
        if self.done {
            return Vec::new();
        }
        let Some(reason) = self.finish else {
            return Vec::new();
        };
        self.done = true;
        let mut out: Vec<StreamItem> = std::mem::take(&mut self.tool_calls)
            .into_values()
            .map(StreamItem::ToolCall)
            .collect();
        out.push(StreamItem::Done(reason));
        out
    }
}
