//! Model backends and supervision for Nebula.
//!
//! - [`backend::LlamaServerBackend`] talks to llama-server (`/v1/chat/completions` with SSE,
//!   `/v1/embeddings`, `/tokenize`, `/health`) and logs a `model.call` event per chat, with the
//!   full request and output in the blob store.
//! - [`supervisor::ModelManager`] keeps one server running per profile: launch inside a Job
//!   Object, health loop, restart with backoff, `Failed` after repeated crashes, profile
//!   switches that drain in-flight requests.
//!
//! The chat model and the CPU embedding server are separate servers, so the daemon runs one
//! `ModelManager` for each.

pub mod backend;
pub mod config;
pub mod launcher;
pub mod sse;
pub mod supervisor;
pub mod types;

pub use backend::{Activity, LlamaServerBackend, ModelBackend};
pub use config::{ModelConfig, ModelProfile, ReasoningStyle, Sampling};
pub use launcher::{LaunchSpec, Launcher, ProcessLauncher, ServerProcess};
pub use supervisor::{ModelManager, Preflight, SupervisorConfig};
pub use types::{BackendHealth, ChatOutput, ChatRequest, ChatStream, StreamItem, ToolCall};

/// Model backend and supervisor failures.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// No profile with that name.
    #[error("unknown model profile {0:?}")]
    UnknownProfile(String),
    /// Inconsistent configuration.
    #[error("model config: {0}")]
    Config(String),
    /// The server isn't ready (starting, restarting, stopped, ...).
    #[error("model unavailable: {0}")]
    Unavailable(String),
    /// The server could not be kept running.
    #[error("model failed: {0}")]
    Failed(String),
    /// A pre-launch check (e.g. commit headroom) refused the launch.
    #[error("launch refused: {0}")]
    Preflight(String),
    /// The process could not be started.
    #[error("launch: {0}")]
    Launch(String),
    /// Connection-level failure.
    #[error("transport: {0}")]
    Transport(String),
    /// A request timed out.
    #[error("request timed out")]
    Timeout,
    /// Non-success HTTP status.
    #[error("HTTP {status}: {body}")]
    Http {
        /// Status code.
        status: u16,
        /// Start of the response body.
        body: String,
    },
    /// The server sent something unexpected.
    #[error("protocol: {0}")]
    Protocol(String),
}

/// The first `max` bytes of `s`, cut at a character boundary.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
