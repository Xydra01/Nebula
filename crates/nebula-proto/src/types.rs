//! Payload types shared by methods and events.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::ids::{SpanId, TraceId};

/// Who wrote a chat message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Instructions; Bonsai accepts exactly one, first.
    System,
    /// The human (or the orchestrator acting for them).
    User,
    /// The model.
    Assistant,
    /// A tool result.
    Tool,
}

/// One chat message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    /// Author.
    pub role: Role,
    /// Text content.
    pub content: String,
}

/// Reasoning effort for Bonsai. `high` is deliberately absent: the server rejects it (HTTP 500).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// No reasoning; instruct sampling preset.
    None,
    /// Short reasoning; thinking preset.
    Low,
    /// Default for planner, verifier and reflector roles.
    Medium,
    /// Longest reasoning.
    Xhigh,
}

/// Token counts and timings for one model call, as reported by llama-server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Prompt tokens processed (not counting cache hits).
    pub prompt_n: u32,
    /// Prompt processing time in milliseconds.
    pub prompt_ms: f64,
    /// Tokens generated.
    pub predicted_n: u32,
    /// Generation time in milliseconds.
    pub predicted_ms: f64,
}

/// Why generation stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// End of turn.
    Stop,
    /// Hit `max_tokens`.
    Length,
    /// The model called a tool.
    ToolCalls,
    /// Cancelled by `chat.cancel` or shutdown.
    Cancelled,
}

/// Lifecycle state of the model server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelState {
    /// Not running.
    Stopped,
    /// Launched, waiting for `/health`.
    Starting,
    /// Healthy and idle.
    Ready,
    /// Healthy and serving a request.
    Busy,
    /// Crashed or unresponsive; waiting out the backoff before relaunching.
    Restarting,
    /// Too many failures in the window; needs attention (`doctor` reports it).
    Failed,
    /// Deliberately unloaded to free VRAM (game mode, Phase 1).
    Unloaded,
}

/// Result of `model.status` and `model.set_profile`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelStatus {
    /// Active profile name (`standard`, `long`, `lean`, `vision`, ...).
    pub profile: String,
    /// Current state.
    pub state: ModelState,
    /// When the server entered `state`.
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    /// Restarts in the current failure window.
    pub restarts: u32,
    /// Last error, if the server failed or restarted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Free/total space on one volume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskUsage {
    /// Mount point, e.g. `F:\`.
    pub mount: String,
    /// Free bytes.
    pub free_bytes: u64,
    /// Total bytes.
    pub total_bytes: u64,
}

/// A process holding GPU memory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpuProcess {
    /// Process ID.
    pub pid: u32,
    /// Executable name.
    pub name: String,
    /// Dedicated GPU memory in MiB.
    pub vram_mib: u64,
}

/// One resource sample. Published every 2 s on the event bus.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSnapshot {
    /// Sample time.
    #[serde(with = "time::serde::rfc3339")]
    pub taken_at: OffsetDateTime,
    /// VRAM in use (all processes), MiB.
    pub vram_used_mib: u64,
    /// Total VRAM, MiB.
    pub vram_total_mib: u64,
    /// GPU utilization, percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_util_pct: Option<u8>,
    /// Physical RAM in use, MiB.
    pub ram_used_mib: u64,
    /// Physical RAM, MiB.
    pub ram_total_mib: u64,
    /// Commit charge, MiB. On Windows, llama-server commits about as much as its VRAM use, so
    /// this, not free RAM, is what runs out first.
    pub commit_used_mib: u64,
    /// Commit limit (RAM + current page file size), MiB.
    pub commit_limit_mib: u64,
    /// Overall CPU utilization, percent.
    pub cpu_pct: f32,
    /// Watched volumes.
    pub disks: Vec<DiskUsage>,
    /// Largest GPU memory users, when the platform reports them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gpu_processes: Vec<GpuProcess>,
}

/// Outcome of one doctor check, ordered from best to worst.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// Healthy.
    Ok,
    /// Works, but needs attention.
    Warn,
    /// Broken.
    Fail,
}

/// One doctor check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorCheck {
    /// Check name, e.g. `model.server`.
    pub name: String,
    /// Result.
    pub status: CheckStatus,
    /// Human-readable detail.
    pub detail: String,
}

/// Result of `doctor.run`. `nebula doctor` exits 0/1/2 for ok/warn/fail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorReport {
    /// Worst status across `checks`.
    pub overall: CheckStatus,
    /// Individual checks.
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// Builds a report whose `overall` is the worst check status (`Ok` when empty).
    #[must_use]
    pub fn from_checks(checks: Vec<DoctorCheck>) -> Self {
        let overall = checks
            .iter()
            .map(|c| c.status)
            .max()
            .unwrap_or(CheckStatus::Ok);
        Self { overall, checks }
    }
}

/// Log severity, ordered from least to most severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Very detailed.
    Trace,
    /// Diagnostic.
    Debug,
    /// Normal operation.
    Info,
    /// Something unexpected that was handled.
    Warn,
    /// A failure.
    Error,
}

/// One structured log event, as written to the JSONL log and streamed by `logs.subscribe`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogEvent {
    /// Event time.
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    /// Severity.
    pub level: LogLevel,
    /// Emitting module, e.g. `nebula_model::supervisor`.
    pub target: String,
    /// Event name, e.g. `model.call` or `sys.daemon_start`.
    pub event: String,
    /// Trace the event belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<TraceId>,
    /// Span the event was emitted in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<SpanId>,
    /// Parent of `span_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<SpanId>,
    /// Task, once tasks exist (Phase 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Step within the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// Event-specific fields. Large payloads appear as blob references, not inline.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
}
