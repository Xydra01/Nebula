//! The Hydrogen inference engine backend, communicating via Win32 named shared memory IPC.
//!
//! [`HydrogenBackend`] implements [`ModelBackend`](crate::backend::ModelBackend) by writing
//! request frames into the daemon→engine ring buffer and reading token/tool-call response
//! frames from the engine→daemon ring buffer. The shared memory protocol is defined in
//! `hydrogen/include/hydrogen_ipc.h` and mirrored in [`nebula_hydrogen`].
//!
//! # Memory ordering
//!
//! The `active_tool_processes` counter in the global shared memory header is incremented
//! with `Release` ordering when a tool process spawns (by the daemon's
//! [`ShmHandle::tool_process_spawned`](nebula_hydrogen::ShmHandle::tool_process_spawned))
//! and loaded with `Acquire` ordering in the engine's dispatch loop. This pairing prevents
//! MSVC `/O2` from reordering the store past subsequent kernel dispatches, ensuring the
//! engine transitions to `cudaDeviceScheduleYield` before the next batch.

use std::sync::Arc;

use async_trait::async_trait;
use nebula_hydrogen::{
    DEFAULT_DAEMON_EVENT_NAME, DEFAULT_ENGINE_EVENT_NAME, DEFAULT_SHM_NAME, FrameFlags,
    MessageType, ShmHandle,
};
use nebula_proto::{ReasoningEffort, StopReason};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::ModelError;
use crate::backend::{Activity, ModelBackend};
use crate::types::{BackendHealth, ChatRequest, ChatStream, StreamItem, ToolCall};

/// Timeout for health/ping probes (ms).
const HEALTH_TIMEOUT_MS: u32 = 2_000;

/// Timeout for waiting on response frames during chat streaming (ms).
const STREAM_WAIT_TIMEOUT_MS: u32 = 120_000;

/// The Hydrogen inference engine backend, connected via Win32 named shared memory.
///
/// Unlike [`LlamaServerBackend`](crate::backend::LlamaServerBackend) which communicates over
/// HTTP SSE, this backend uses a zero-copy 64 MiB shared memory ring buffer with Win32
/// event signaling for sub-50 µs token delivery latency.
pub struct HydrogenBackend {
    /// The shared memory handle (owned, maps the 64 MiB section).
    shm: Arc<ShmHandle>,
    /// Activity tracking (in-flight count + cancellation).
    activity: Activity,
    /// Next stream id for request framing.
    next_stream_id: std::sync::atomic::AtomicU32,
}

impl HydrogenBackend {
    /// Opens (or creates) the shared memory IPC section and both Win32 events.
    ///
    /// When `create` is true, the section is created and the header initialized (daemon-side).
    /// When false, an existing section created by the engine is opened.
    ///
    /// # Errors
    /// [`ModelError::Transport`] on Win32 API failure or protocol mismatch.
    pub fn open(create: bool) -> Result<Self, ModelError> {
        Self::open_named(
            DEFAULT_SHM_NAME,
            DEFAULT_ENGINE_EVENT_NAME,
            DEFAULT_DAEMON_EVENT_NAME,
            create,
        )
    }

    /// Opens with custom shared memory and event names (useful for testing).
    ///
    /// # Errors
    /// [`ModelError::Transport`] on Win32 API failure or protocol mismatch.
    pub fn open_named(
        shm_name: &str,
        engine_event_name: &str,
        daemon_event_name: &str,
        create: bool,
    ) -> Result<Self, ModelError> {
        let shm = ShmHandle::open(shm_name, engine_event_name, daemon_event_name, create)
            .map_err(|e| ModelError::Transport(format!("hydrogen IPC: {e}")))?;

        let shm = Arc::new(shm);
        nebula_hydrogen::register_global_shm(Arc::clone(&shm));

        Ok(Self {
            shm,
            activity: Activity::new(),
            next_stream_id: std::sync::atomic::AtomicU32::new(1),
        })
    }

    /// The shared memory handle, for direct access to `active_tool_processes`.
    #[must_use]
    pub fn shm(&self) -> &Arc<ShmHandle> {
        &self.shm
    }

    /// The request-tracking handle shared with the supervisor.
    #[must_use]
    pub fn activity(&self) -> &Activity {
        &self.activity
    }

    /// Allocates a unique stream id for a new request.
    fn alloc_stream_id(&self) -> u32 {
        self.next_stream_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Builds the JSON payload for a `MSG_GENERATE_REQUEST` frame from a [`ChatRequest`].
    fn build_generate_payload(req: &ChatRequest) -> Vec<u8> {
        let effort: u8 = match req.reasoning {
            ReasoningEffort::None => 0,
            ReasoningEffort::Low => 1,
            ReasoningEffort::Medium => 2,
            ReasoningEffort::Xhigh => 3,
        };
        let body = json!({
            "messages": req.messages,
            "max_tokens": req.max_tokens,
            "reasoning_effort": effort,
            "tools": req.tools,
            "response_schema": req.response_schema,
        });
        serde_json::to_vec(&body).unwrap_or_default()
    }

    /// Reads response frames from the ring buffer and sends them into the channel.
    async fn pump_responses(
        shm: Arc<ShmHandle>,
        stream_id: u32,
        tx: mpsc::Sender<Result<StreamItem, ModelError>>,
        cancel: CancellationToken,
    ) {
        loop {
            // Check cancellation.
            if cancel.is_cancelled() {
                let _ = tx.send(Ok(StreamItem::Done(StopReason::Cancelled))).await;
                return;
            }
            if tx.is_closed() {
                return;
            }

            // Wait for the engine event (response data ready).
            match shm.wait_engine_event(STREAM_WAIT_TIMEOUT_MS) {
                Ok(()) => {}
                Err(_timeout) => {
                    let _ = tx.send(Err(ModelError::Timeout)).await;
                    return;
                }
            }

            // Drain all available frames.
            while let Some(frame) = shm.peek_response_frame() {
                // Skip frames for other streams.
                if frame.stream_id != stream_id {
                    // Advance past the frame anyway to avoid blocking.
                    let _ = shm.read_response_payload(frame.payload_size as usize);
                    continue;
                }

                let payload = match shm.read_response_payload(frame.payload_size as usize) {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = tx
                            .send(Err(ModelError::Protocol(format!("ring read: {e}"))))
                            .await;
                        return;
                    }
                };

                let Some(msg_type) = MessageType::from_u16(frame.msg_type) else {
                    tracing::warn!(
                        event = "hydrogen.unknown_msg",
                        msg_type = frame.msg_type,
                        "skipping unknown message type"
                    );
                    continue;
                };

                match msg_type {
                    MessageType::TokenChunk => {
                        if let Some(token) = nebula_hydrogen::TokenPayload::from_bytes(&payload) {
                            let text = token.text_str().to_owned();
                            if !text.is_empty() {
                                let item = StreamItem::Token {
                                    text,
                                    reasoning: false,
                                };
                                if tx.send(Ok(item)).await.is_err() {
                                    return;
                                }
                            }
                            if token.is_final != 0 {
                                let _ = tx.send(Ok(StreamItem::Done(StopReason::Stop))).await;
                                return;
                            }
                        }
                    }
                    MessageType::ToolCall => {
                        // Parse tool call from JSON payload.
                        if let Ok(tc) =
                            std::str::from_utf8(&payload).map_err(|_| ()).and_then(|t| {
                                serde_json::from_str::<serde_json::Value>(t).map_err(|_| ())
                            })
                        {
                            let tool_call = ToolCall {
                                id: tc
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_owned(),
                                name: tc
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_owned(),
                                arguments: tc
                                    .get("arguments")
                                    .map(ToString::to_string)
                                    .unwrap_or_default(),
                            };
                            if tx.send(Ok(StreamItem::ToolCall(tool_call))).await.is_err() {
                                return;
                            }
                        }
                    }
                    _ => {
                        // Ignore pong, wrap markers, etc.
                    }
                }

                // If END_OF_STREAM flag is set, the stream is done.
                if frame.flags & FrameFlags::END_OF_STREAM != 0 {
                    let _ = tx.send(Ok(StreamItem::Done(StopReason::Stop))).await;
                    return;
                }
            }
        }
    }
}

impl std::fmt::Debug for HydrogenBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HydrogenBackend")
            .field("shm_size", &self.shm.size())
            .field("active_tool_processes", &self.shm.active_tool_processes())
            .finish()
    }
}

impl Drop for HydrogenBackend {
    fn drop(&mut self) {
        nebula_hydrogen::unregister_global_shm();
    }
}

#[async_trait]
impl ModelBackend for HydrogenBackend {
    #[tracing::instrument(level = "debug", skip_all)]
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, ModelError> {
        if self.activity.is_cancelled() {
            return Err(ModelError::Unavailable(
                "hydrogen backend is stopping".into(),
            ));
        }

        let stream_id = self.alloc_stream_id();
        let payload = Self::build_generate_payload(&req);

        self.shm
            .write_request_frame(
                MessageType::GenerateRequest,
                FrameFlags::NONE,
                stream_id,
                &payload,
            )
            .map_err(|e| ModelError::Transport(format!("write request: {e}")))?;

        self.shm
            .signal_daemon_event()
            .map_err(|e| ModelError::Transport(format!("signal: {e}")))?;

        let (tx, rx) = mpsc::channel(64);
        let shm = Arc::clone(&self.shm);
        let cancel = self.activity.cancel_token();

        let _guard = self.activity.enter();
        tokio::spawn(async move {
            let _g = _guard;
            Self::pump_responses(shm, stream_id, tx, cancel).await;
        });

        Ok(ChatStream::new(rx))
    }

    async fn embed(&self, _input: Vec<String>) -> Result<Vec<Vec<f32>>, ModelError> {
        // Hydrogen does not support embeddings (those use the CPU embedding server).
        Err(ModelError::Unavailable(
            "hydrogen backend does not support embeddings".into(),
        ))
    }

    async fn tokenize(&self, _text: &str) -> Result<Vec<u32>, ModelError> {
        // Tokenization is handled by the engine internally; not exposed over IPC yet.
        Err(ModelError::Unavailable(
            "hydrogen backend does not support tokenize over IPC".into(),
        ))
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn health(&self) -> Result<BackendHealth, ModelError> {
        self.shm
            .ping(HEALTH_TIMEOUT_MS)
            .map_err(|e| ModelError::Transport(format!("hydrogen ping: {e}")))?;
        Ok(BackendHealth::Ok)
    }
}
