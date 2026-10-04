//! The [`ModelBackend`] trait and its llama-server implementation.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use nebula_proto::{StopReason, Usage};
use nebula_telemetry::BlobStore;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::ModelProfile;
use crate::sse::ChunkParser;
use crate::types::{BackendHealth, ChatRequest, ChatStream, StreamItem, ToolCall};
use crate::{ModelError, truncate};

/// A model server Nebula can talk to.
#[async_trait]
pub trait ModelBackend: Send + Sync {
    /// Starts a streaming chat completion.
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, ModelError>;
    /// Embeds each input.
    async fn embed(&self, input: Vec<String>) -> Result<Vec<Vec<f32>>, ModelError>;
    /// Tokenizes text with the loaded model's vocabulary.
    async fn tokenize(&self, text: &str) -> Result<Vec<u32>, ModelError>;
    /// Probes the server.
    async fn health(&self) -> Result<BackendHealth, ModelError>;
}

/// Requests in flight on one server, plus the token that cancels them all.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    in_flight: Arc<AtomicUsize>,
    cancel: CancellationToken,
}

impl Activity {
    /// A fresh tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests currently streaming.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// Cancels every in-flight request; they end with `Done(Cancelled)`.
    pub fn cancel_all(&self) {
        self.cancel.cancel();
    }

    /// Whether [`Activity::cancel_all`] was called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn enter(&self) -> ActivityGuard {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        ActivityGuard(Arc::clone(&self.in_flight))
    }
}

struct ActivityGuard(Arc<AtomicUsize>);

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Talks to one llama-server instance over its OpenAI-compatible HTTP API.
#[derive(Clone, Debug)]
pub struct LlamaServerBackend {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    profile_name: String,
    profile: ModelProfile,
    blobs: Option<BlobStore>,
    activity: Activity,
}

/// Timeout for `/health`, `/tokenize` and `/v1/embeddings`.
const SHORT_TIMEOUT: Duration = Duration::from_secs(30);

impl LlamaServerBackend {
    /// A client for the server at `base_url` (e.g. `http://127.0.0.1:8080`).
    ///
    /// # Errors
    /// [`ModelError::Transport`] if the HTTP client can't be built.
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        profile_name: impl Into<String>,
        profile: ModelProfile,
        blobs: Option<BlobStore>,
        activity: Activity,
    ) -> Result<Self, ModelError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| ModelError::Transport(e.to_string()))?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            profile_name: profile_name.into(),
            profile,
            blobs,
            activity,
        })
    }

    /// The request-tracking handle shared with the supervisor.
    #[must_use]
    pub fn activity(&self) -> &Activity {
        &self.activity
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let rb = self
            .client
            .request(method, format!("{}{path}", self.base_url));
        match &self.api_key {
            Some(k) => rb.bearer_auth(k),
            None => rb,
        }
    }

    /// The JSON body sent to `/v1/chat/completions`.
    #[must_use]
    pub fn chat_body(&self, req: &ChatRequest) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("messages".into(), json!(req.messages));
        body.insert("max_tokens".into(), json!(req.max_tokens));
        body.insert("cache_prompt".into(), json!(true));
        body.insert("stream".into(), json!(true));
        body.extend(self.profile.request_extras(req.reasoning));
        if !req.tools.is_empty() {
            body.insert("tools".into(), json!(req.tools));
        }
        if let Some(schema) = &req.response_schema {
            body.insert(
                "response_format".into(),
                json!({
                    "type": "json_schema",
                    "json_schema": { "name": "response", "schema": schema, "strict": true },
                }),
            );
        }
        Value::Object(body)
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response, ModelError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        Err(ModelError::Http {
            status: status.as_u16(),
            body: truncate(&body, 500),
        })
    }
}

fn transport(e: &reqwest::Error) -> ModelError {
    if e.is_timeout() {
        ModelError::Timeout
    } else {
        ModelError::Transport(e.to_string())
    }
}

#[async_trait]
impl ModelBackend for LlamaServerBackend {
    #[tracing::instrument(level = "debug", skip_all, fields(profile = %self.profile_name))]
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, ModelError> {
        if self.profile.embedding {
            return Err(ModelError::Unavailable(format!(
                "profile {} serves embeddings, not chat",
                self.profile_name
            )));
        }
        if self.activity.is_cancelled() {
            return Err(ModelError::Unavailable("server is stopping".into()));
        }
        let body = self.chat_body(&req);
        let guard = self.activity.enter();
        let resp = self
            .request(reqwest::Method::POST, "/v1/chat/completions")
            .json(&body)
            .send()
            .await
            .map_err(|e| transport(&e))?;
        let resp = Self::check(resp).await?;
        let (tx, rx) = mpsc::channel(64);
        let call = CallLog {
            profile: self.profile_name.clone(),
            trace_id: req.trace_id.map(|t| t.to_string()),
            body,
            blobs: self.blobs.clone(),
            started: Instant::now(),
        };
        let cancel = self.activity.cancel.clone();
        tokio::spawn(async move {
            let _guard = guard;
            let summary = pump(resp, &tx, &cancel).await;
            call.log(&summary);
        });
        Ok(ChatStream::new(rx))
    }

    #[tracing::instrument(level = "debug", skip_all, fields(n = input.len()))]
    async fn embed(&self, input: Vec<String>) -> Result<Vec<Vec<f32>>, ModelError> {
        #[derive(Deserialize)]
        struct Resp {
            data: Vec<Item>,
        }
        #[derive(Deserialize)]
        struct Item {
            #[serde(default)]
            index: usize,
            embedding: Vec<f32>,
        }
        let resp = self
            .request(reqwest::Method::POST, "/v1/embeddings")
            .timeout(SHORT_TIMEOUT)
            .json(&json!({ "input": input }))
            .send()
            .await
            .map_err(|e| transport(&e))?;
        let mut parsed: Resp = Self::check(resp)
            .await?
            .json()
            .await
            .map_err(|e| ModelError::Protocol(e.to_string()))?;
        parsed.data.sort_by_key(|i| i.index);
        Ok(parsed.data.into_iter().map(|i| i.embedding).collect())
    }

    async fn tokenize(&self, text: &str) -> Result<Vec<u32>, ModelError> {
        #[derive(Deserialize)]
        struct Resp {
            tokens: Vec<u32>,
        }
        let resp = self
            .request(reqwest::Method::POST, "/tokenize")
            .timeout(SHORT_TIMEOUT)
            .json(&json!({ "content": text }))
            .send()
            .await
            .map_err(|e| transport(&e))?;
        let parsed: Resp = Self::check(resp)
            .await?
            .json()
            .await
            .map_err(|e| ModelError::Protocol(e.to_string()))?;
        Ok(parsed.tokens)
    }

    async fn health(&self) -> Result<BackendHealth, ModelError> {
        let resp = self
            .request(reqwest::Method::GET, "/health")
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|e| transport(&e))?;
        match resp.status().as_u16() {
            200 => Ok(BackendHealth::Ok),
            503 => Ok(BackendHealth::Loading),
            _ => Self::check(resp).await.map(|_| BackendHealth::Ok),
        }
    }
}

#[derive(Default)]
struct CallSummary {
    text: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<Usage>,
    stop: Option<StopReason>,
    error: Option<String>,
}

impl CallSummary {
    fn record(&mut self, item: &StreamItem) {
        match item {
            StreamItem::Token { text, reasoning } => {
                if *reasoning {
                    self.reasoning.push_str(text);
                } else {
                    self.text.push_str(text);
                }
            }
            StreamItem::ToolCall(c) => self.tool_calls.push(c.clone()),
            StreamItem::Usage(u) => self.usage = Some(*u),
            StreamItem::Done(r) => self.stop = Some(*r),
        }
    }
}

/// Reads SSE events into the channel until done, cancelled, or the receiver is dropped.
async fn pump(
    resp: reqwest::Response,
    tx: &mpsc::Sender<Result<StreamItem, ModelError>>,
    cancel: &CancellationToken,
) -> CallSummary {
    let mut events = resp.bytes_stream().eventsource();
    let mut parser = ChunkParser::new();
    let mut summary = CallSummary::default();
    loop {
        let next = tokio::select! {
            () = cancel.cancelled() => {
                let item = StreamItem::Done(StopReason::Cancelled);
                summary.record(&item);
                let _ = tx.send(Ok(item)).await;
                return summary;
            }
            () = tx.closed() => {
                summary.stop = Some(StopReason::Cancelled);
                return summary;
            }
            ev = events.next() => ev,
        };
        let items = match next {
            Some(Ok(ev)) => parser.feed(&ev.data),
            Some(Err(e)) => Err(ModelError::Transport(e.to_string())),
            None => {
                let items = parser.finish();
                if items.is_empty() {
                    Err(ModelError::Protocol(
                        "stream ended without a finish reason".into(),
                    ))
                } else {
                    Ok(items)
                }
            }
        };
        match items {
            Ok(items) => {
                for item in items {
                    summary.record(&item);
                    if tx.send(Ok(item)).await.is_err() {
                        summary.stop.get_or_insert(StopReason::Cancelled);
                        return summary;
                    }
                }
                if parser.is_done() {
                    return summary;
                }
            }
            Err(e) => {
                summary.error = Some(e.to_string());
                let _ = tx.send(Err(e)).await;
                return summary;
            }
        }
    }
}

/// Context for the `model.call` log event written when a chat ends.
struct CallLog {
    profile: String,
    trace_id: Option<String>,
    body: Value,
    blobs: Option<BlobStore>,
    started: Instant,
}

impl CallLog {
    fn log(&self, s: &CallSummary) {
        let put = |v: &Value| -> Option<String> {
            let blobs = self.blobs.as_ref()?;
            let bytes = serde_json::to_vec(v).ok()?;
            match blobs.put(&bytes) {
                Ok(r) => Some(r.to_string()),
                Err(e) => {
                    tracing::warn!(event = "blob.write_failed", error = %e);
                    None
                }
            }
        };
        let prompt_blob = put(&self.body);
        let calls: Vec<Value> = s
            .tool_calls
            .iter()
            .map(|c| json!({ "id": c.id, "name": c.name, "arguments": c.arguments }))
            .collect();
        let output_blob = put(&json!({
            "text": s.text,
            "reasoning": s.reasoning,
            "tool_calls": calls,
        }));
        let u = s.usage.unwrap_or_default();
        let stop = s
            .stop
            .and_then(|r| serde_json::to_value(r).ok())
            .and_then(|v| v.as_str().map(str::to_owned));
        let wall_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let outcome = match (&s.error, s.stop) {
            (Some(_), _) => "error",
            (None, Some(StopReason::Cancelled)) => "cancelled",
            (None, _) => "ok",
        };
        tracing::info!(
            event = "model.call",
            trace_id = self.trace_id.as_deref(),
            profile = %self.profile,
            outcome,
            stop_reason = stop.as_deref(),
            error = s.error.as_deref(),
            prompt_n = u.prompt_n,
            prompt_ms = u.prompt_ms,
            predicted_n = u.predicted_n,
            predicted_ms = u.predicted_ms,
            wall_ms,
            prompt_blob = prompt_blob.as_deref(),
            output_blob = output_blob.as_deref(),
        );
    }
}
