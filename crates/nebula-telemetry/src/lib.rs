//! Structured telemetry for Nebula.
//!
//! [`init`] installs a `tracing` subscriber whose single layer turns every event into a
//! [`nebula_proto::LogEvent`], redacts it, and sends it to:
//!
//! - the daily JSONL file `<log_dir>\nebula-YYYY-MM-DD.jsonl` (UTC dates),
//! - the console (stderr), when enabled,
//! - a broadcast channel that feeds `logs.subscribe`.
//!
//! Conventions for emitters:
//!
//! - Name events with an `event` field: `tracing::info!(event = "model.call", ...)`.
//! - Open a trace with a span carrying `trace_id = %id`; child spans and their events
//!   inherit it, as well as `task_id` / `step_id`.
//! - Put large payloads in the [`BlobStore`] and log the [`BlobRef`] instead.

pub mod blobs;
pub mod budget;
pub mod layer;
pub mod redact;
pub mod writer;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nebula_proto::LogEvent;
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::Subscriber;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Registry};

pub use blobs::{BlobError, BlobRef, BlobStore};
pub use budget::LogUsage;
pub use layer::NebulaLayer;
pub use redact::Redactor;

fn default_filter() -> String {
    "info".into()
}

const fn default_broadcast_capacity() -> usize {
    1024
}

/// Telemetry settings (the `[telemetry]` section of the daemon config).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// Directory for daily logs and `blobs\`. `None` disables the file sink and blob store
    /// (used by short-lived CLI commands).
    #[serde(default)]
    pub log_dir: Option<PathBuf>,
    /// `EnvFilter` directives applied to every sink, e.g. `"info,nebula_model=debug"`.
    #[serde(default = "default_filter")]
    pub filter: String,
    /// Print events to stderr.
    #[serde(default)]
    pub console: bool,
    /// Events buffered per `logs.subscribe` listener before it starts missing events.
    #[serde(default = "default_broadcast_capacity")]
    pub broadcast_capacity: usize,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            log_dir: None,
            filter: default_filter(),
            console: false,
            broadcast_capacity: default_broadcast_capacity(),
        }
    }
}

/// Telemetry setup failures.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// The filter string did not parse.
    #[error("invalid log filter {filter:?}: {message}")]
    Filter {
        /// The rejected filter.
        filter: String,
        /// Parser message.
        message: String,
    },
    /// A global subscriber was already installed.
    #[error("a global tracing subscriber is already set")]
    AlreadyInitialized,
    /// Disk accounting failed.
    #[error(transparent)]
    Budget(#[from] budget::BudgetError),
}

/// Handle to the running telemetry: the redactor, blob store and log broadcast.
///
/// Every event is flushed to disk as it is written, so dropping this does not lose logs.
#[derive(Clone, Debug)]
pub struct Telemetry {
    redactor: Arc<Redactor>,
    blobs: Option<BlobStore>,
    events: broadcast::Sender<LogEvent>,
    log_dir: Option<PathBuf>,
}

impl Telemetry {
    /// The redactor shared by every sink. Register secrets here as they are loaded.
    #[must_use]
    pub fn redactor(&self) -> &Arc<Redactor> {
        &self.redactor
    }

    /// The blob store, if a log directory is configured.
    #[must_use]
    pub fn blobs(&self) -> Option<&BlobStore> {
        self.blobs.as_ref()
    }

    /// A new receiver of every log event from now on (for `logs.subscribe`).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LogEvent> {
        self.events.subscribe()
    }

    /// The log directory, if configured.
    #[must_use]
    pub fn log_dir(&self) -> Option<&Path> {
        self.log_dir.as_deref()
    }

    /// Current log and blob disk usage (zero without a log directory).
    ///
    /// # Errors
    /// Filesystem errors while measuring.
    pub fn usage(&self) -> Result<LogUsage, TelemetryError> {
        match &self.log_dir {
            Some(dir) => Ok(budget::usage(dir)?),
            None => Ok(LogUsage::default()),
        }
    }
}

/// Builds the subscriber and its handle without installing it, for tests and for callers
/// that scope the subscriber with `tracing::subscriber::with_default`.
///
/// # Errors
/// [`TelemetryError::Filter`] for an invalid filter string.
pub fn build(
    config: &TelemetryConfig,
) -> Result<(Telemetry, impl Subscriber + Send + Sync + 'static), TelemetryError> {
    let filter = EnvFilter::try_new(&config.filter).map_err(|e| TelemetryError::Filter {
        filter: config.filter.clone(),
        message: e.to_string(),
    })?;
    let redactor = Arc::new(Redactor::new());
    let (events, _) = broadcast::channel(config.broadcast_capacity.max(1));
    let blobs = config
        .log_dir
        .as_ref()
        .map(|d| BlobStore::new(d.join("blobs"), Arc::clone(&redactor)));
    let layer = NebulaLayer::new(
        Arc::clone(&redactor),
        config.log_dir.as_ref().map(writer::DailyJsonl::new),
        config.console,
        events.clone(),
    );
    let subscriber = Registry::default().with(filter).with(layer);
    let telemetry = Telemetry {
        redactor,
        blobs,
        events,
        log_dir: config.log_dir.clone(),
    };
    Ok((telemetry, subscriber))
}

/// Builds telemetry and installs it as the global subscriber. Call once, early in `main`.
///
/// # Errors
/// [`TelemetryError::Filter`] or [`TelemetryError::AlreadyInitialized`].
pub fn init(config: &TelemetryConfig) -> Result<Telemetry, TelemetryError> {
    let (telemetry, subscriber) = build(config)?;
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_| TelemetryError::AlreadyInitialized)?;
    Ok(telemetry)
}
