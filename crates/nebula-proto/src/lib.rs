//! Wire types for Nebula's local IPC.
//!
//! The daemon and its clients exchange JSON-RPC 2.0 messages as **newline-delimited JSON** over
//! the named pipe `\\.\pipe\nebula`. Every message carries [`PROTO_VERSION`] so clients and the
//! daemon can be upgraded independently.
//!
//! - [`Request`]: a client call ([`Method`]), answered by exactly one [`Response`].
//! - [`Notification`]: a daemon event ([`Event`]) with no reply.
//! - [`Message`]: any of the three, with [`Message::decode`] / [`Message::encode`] for framing.

mod envelope;
mod events;
mod ids;
mod methods;
mod types;

pub use envelope::{
    JsonRpcVersion, Message, Notification, Outcome, PROTO_VERSION, ProtoError, Request, RequestId,
    Response, RpcError, error_code,
};
pub use events::{ChatDone, ChatError, ChatToken, Event, ModelStateChanged};
pub use ids::{ChatId, SpanId, TraceId};
pub use methods::{
    ChatCancelParams, ChatStartParams, ChatStarted, DaemonStatus, Empty, LogsSubscribeParams,
    Method, ModelSetProfileParams, ToolCallOutcome, ToolInfo, ToolList, ToolsCallParams,
};
pub use types::{
    ChatMessage, CheckStatus, DiskUsage, DoctorCheck, DoctorReport, GpuProcess, LogEvent, LogLevel,
    ModelState, ModelStatus, ReasoningEffort, ResourceSnapshot, Role, StopReason, Usage,
};
