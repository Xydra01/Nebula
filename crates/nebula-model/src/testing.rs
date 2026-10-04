//! A fake llama-server (axum) and a launcher that starts it in-process, for tests in this
//! crate and its users (the daemon). Behind the `test-support` feature.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::{
    LaunchSpec, Launcher, ModelConfig, ModelError, ModelProfile, ReasoningStyle, Sampling,
    ServerProcess,
};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the fake server answers, and what it has received.
#[derive(Default)]
pub struct FakeState {
    /// Required bearer key, set by [`FakeLauncher`] from `LLAMA_API_KEY`.
    pub api_key: Mutex<Option<String>>,
    /// `/health` status code.
    pub health: AtomicU16,
    /// `/v1/chat/completions` status code.
    pub chat_status: AtomicU16,
    /// SSE `data:` payloads streamed by chat, ending with `[DONE]`.
    pub chunks: Mutex<Vec<String>>,
    /// Delay before each chunk.
    pub chunk_delay_ms: AtomicU64,
    /// `(path, body)` of every request.
    pub requests: Mutex<Vec<(String, Value)>>,
}

impl FakeState {
    /// Healthy, answering 200, with no chunks.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let s = Self::default();
        s.health.store(200, Ordering::SeqCst);
        s.chat_status.store(200, Ordering::SeqCst);
        Arc::new(s)
    }

    /// Sets the chat stream (a `[DONE]` is appended).
    pub fn set_chunks(&self, chunks: Vec<Value>) {
        let mut data: Vec<String> = chunks.into_iter().map(|c| c.to_string()).collect();
        data.push("[DONE]".into());
        *lock(&self.chunks) = data;
    }

    /// Sets the delay before each chunk.
    pub fn set_chunk_delay(&self, delay: Duration) {
        self.chunk_delay_ms.store(
            u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
    }

    /// Bodies received on `path`.
    #[must_use]
    pub fn requests(&self, path: &str) -> Vec<Value> {
        lock(&self.requests)
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        match &*lock(&self.api_key) {
            None => true,
            Some(k) => headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v == format!("Bearer {k}")),
        }
    }
}

/// A content delta chunk.
#[must_use]
pub fn content(text: &str) -> Value {
    json!({ "choices": [{ "index": 0, "delta": { "content": text }, "finish_reason": null }] })
}

/// A reasoning delta chunk.
#[must_use]
pub fn reasoning(text: &str) -> Value {
    json!({ "choices": [{ "index": 0, "delta": { "reasoning_content": text }, "finish_reason": null }] })
}

/// The final chunk with a finish reason and timings.
#[must_use]
pub fn finish(reason: &str) -> Value {
    json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }],
        "timings": { "prompt_n": 12, "prompt_ms": 34.5, "predicted_n": 3, "predicted_ms": 6.25,
                     "predicted_per_second": 480.0 }
    })
}

fn status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

async fn health(State(s): State<Arc<FakeState>>, headers: HeaderMap) -> Response {
    if !s.authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let code = status(s.health.load(Ordering::SeqCst));
    (code, axum::Json(json!({ "status": "x" }))).into_response()
}

async fn chat(
    State(s): State<Arc<FakeState>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    if !s.authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    lock(&s.requests).push(("/v1/chat/completions".into(), body));
    let code = s.chat_status.load(Ordering::SeqCst);
    if code != 200 {
        return (
            status(code),
            axum::Json(json!({ "error": { "message": "boom" } })),
        )
            .into_response();
    }
    let chunks = lock(&s.chunks).clone();
    let delay = Duration::from_millis(s.chunk_delay_ms.load(Ordering::SeqCst));
    let stream = futures_util::stream::iter(chunks).then(move |c| async move {
        tokio::time::sleep(delay).await;
        Ok::<_, Infallible>(format!("data: {c}\n\n"))
    });
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn tokenize(
    State(s): State<Arc<FakeState>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    lock(&s.requests).push(("/tokenize".into(), body));
    axum::Json(json!({ "tokens": [1, 2, 3] })).into_response()
}

async fn embeddings(
    State(s): State<Arc<FakeState>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    lock(&s.requests).push(("/v1/embeddings".into(), body));
    axum::Json(json!({ "data": [
        { "index": 1, "embedding": [0.5, 0.5] },
        { "index": 0, "embedding": [0.25, 0.75] },
    ] }))
    .into_response()
}

/// The fake's routes.
pub fn router(state: Arc<FakeState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/chat/completions", post(chat))
        .route("/tokenize", post(tokenize))
        .route("/v1/embeddings", post(embeddings))
        .with_state(state)
}

/// Serves the fake on `listener` in a background task.
pub fn serve(state: Arc<FakeState>, listener: TcpListener) -> JoinHandle<()> {
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(state)).await;
    })
}

/// Serves the fake on a random port and returns its base URL.
///
/// # Errors
/// If binding fails.
pub async fn start(state: Arc<FakeState>) -> std::io::Result<(String, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    Ok((url, serve(state, listener)))
}

/// Controls one fake server "process".
pub struct ProcCtl {
    exited: AtomicBool,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl ProcCtl {
    fn kill(&self) {
        if let Some(t) = lock(&self.task).take() {
            t.abort();
        }
        self.exited.store(true, Ordering::SeqCst);
    }

    /// Whether it has "exited".
    #[must_use]
    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
}

struct FakeProcess(Arc<ProcCtl>);

#[async_trait]
impl ServerProcess for FakeProcess {
    fn pid(&self) -> Option<u32> {
        Some(4242)
    }
    fn try_exit(&mut self) -> Option<String> {
        self.0.exited().then(|| "exit code: 1".to_owned())
    }
    async fn stop(&mut self) {
        self.0.kill();
    }
    fn recent_output(&self) -> Vec<String> {
        vec!["fake llama-server output".into()]
    }
}

/// Starts the fake server in-process on the port the supervisor picked.
pub struct FakeLauncher {
    /// Shared by every server it starts.
    pub state: Arc<FakeState>,
    /// Every launch, in order.
    pub launches: Mutex<Vec<LaunchSpec>>,
    /// Make the next launches exit immediately.
    pub crash_on_start: AtomicBool,
    /// The latest server.
    pub current: Mutex<Option<Arc<ProcCtl>>>,
}

impl FakeLauncher {
    /// A launcher serving `state`.
    #[must_use]
    pub fn new(state: Arc<FakeState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            launches: Mutex::new(Vec::new()),
            crash_on_start: AtomicBool::new(false),
            current: Mutex::new(None),
        })
    }

    /// Simulates the server process dying.
    pub fn crash(&self) {
        if let Some(c) = &*lock(&self.current) {
            c.kill();
        }
    }

    /// The latest server, if one was launched.
    #[must_use]
    pub fn current(&self) -> Option<Arc<ProcCtl>> {
        lock(&self.current).clone()
    }

    /// Every launch so far.
    #[must_use]
    pub fn launches(&self) -> Vec<LaunchSpec> {
        lock(&self.launches).clone()
    }
}

#[async_trait]
impl Launcher for FakeLauncher {
    async fn launch(&self, spec: LaunchSpec) -> Result<Box<dyn ServerProcess>, ModelError> {
        lock(&self.launches).push(spec.clone());
        let key = spec
            .env
            .iter()
            .find(|(k, _)| k == "LLAMA_API_KEY")
            .map(|(_, v)| v.clone());
        *lock(&self.state.api_key) = key;
        let ctl = Arc::new(ProcCtl {
            exited: AtomicBool::new(false),
            task: Mutex::new(None),
        });
        if self.crash_on_start.load(Ordering::SeqCst) {
            ctl.exited.store(true, Ordering::SeqCst);
        } else {
            let listener = TcpListener::bind(("127.0.0.1", spec.port))
                .await
                .map_err(|e| ModelError::Launch(e.to_string()))?;
            *lock(&ctl.task) = Some(serve(Arc::clone(&self.state), listener));
        }
        *lock(&self.current) = Some(Arc::clone(&ctl));
        Ok(Box::new(FakeProcess(ctl)))
    }
}

/// A small profile for `model` on the `fake` runtime.
#[must_use]
pub fn profile(model: &str) -> ModelProfile {
    let mut instruct = serde_json::Map::new();
    instruct.insert("temperature".into(), json!(0.7));
    let mut thinking = serde_json::Map::new();
    thinking.insert("temperature".into(), json!(1.0));
    ModelProfile {
        runtime: "fake".into(),
        model: model.into(),
        ctx: 4096,
        kv_type: "f16".into(),
        kv_bias: None,
        flags: vec!["-ngl".into(), "99".into()],
        reasoning_style: ReasoningStyle::Effort,
        sampling: Sampling { thinking, instruct },
        embedding: false,
        commit_estimate_mib: None,
    }
}

/// Profiles `a` (default) and `b` on the `fake` runtime.
#[must_use]
pub fn config() -> ModelConfig {
    ModelConfig {
        default_profile: "a".into(),
        runtimes: BTreeMap::from([("fake".into(), "fake-llama-server.exe".into())]),
        profiles: BTreeMap::from([
            ("a".into(), profile("a.gguf")),
            ("b".into(), profile("b.gguf")),
        ]),
    }
}
