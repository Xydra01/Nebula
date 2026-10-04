//! The `tracing` layer that turns events into [`LogEvent`]s, writes them as JSONL and
//! broadcasts them to `logs.subscribe` listeners.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use nebula_proto::{LogEvent, LogLevel, SpanId, TraceId};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

use crate::redact::Redactor;
use crate::writer::DailyJsonl;

/// Span field that sets the trace for the span and its descendants.
pub const TRACE_ID_FIELD: &str = "trace_id";
/// Span field naming the task (Phase 1).
pub const TASK_ID_FIELD: &str = "task_id";
/// Span field naming the step within a task.
pub const STEP_ID_FIELD: &str = "step_id";
/// Event field holding the event name, e.g. `event = "model.call"`.
pub const EVENT_FIELD: &str = "event";
/// Event name used when an event has no `event` field.
pub const DEFAULT_EVENT: &str = "log";

/// Per-span context stored in the registry's span extensions.
#[derive(Clone, Debug)]
struct SpanCtx {
    span_id: SpanId,
    parent_span_id: Option<SpanId>,
    trace_id: Option<TraceId>,
    task_id: Option<String>,
    step_id: Option<String>,
}

#[derive(Default)]
struct FieldVisitor {
    fields: Map<String, Value>,
}

impl FieldVisitor {
    fn put(&mut self, field: &Field, value: Value) {
        self.fields.insert(field.name().to_owned(), value);
    }
}

impl Visit for FieldVisitor {
    fn record_f64(&mut self, field: &Field, value: f64) {
        let v = serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number);
        self.put(field, v);
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::from(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::from(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, Value::from(value));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, Value::from(value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, Value::from(format!("{value:?}")));
    }
}

fn take_string(fields: &mut Map<String, Value>, key: &str) -> Option<String> {
    match fields.remove(key)? {
        Value::String(s) => Some(s),
        other => Some(other.to_string()),
    }
}

fn level_of(level: Level) -> LogLevel {
    match level {
        Level::TRACE => LogLevel::Trace,
        Level::DEBUG => LogLevel::Debug,
        Level::INFO => LogLevel::Info,
        Level::WARN => LogLevel::Warn,
        Level::ERROR => LogLevel::Error,
    }
}

/// One human-readable line for the console, built from the already-redacted event.
#[must_use]
pub fn console_line(e: &LogEvent) -> String {
    use std::fmt::Write as _;
    let t = e.ts.time();
    let level = match e.level {
        LogLevel::Trace => "TRACE",
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => " INFO",
        LogLevel::Warn => " WARN",
        LogLevel::Error => "ERROR",
    };
    let mut line = format!(
        "{:02}:{:02}:{:02}.{:03} {level} {}: {}",
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond(),
        e.target,
        e.event
    );
    if let Some(Value::String(m)) = e.fields.get("message") {
        let _ = write!(line, " {m}");
    }
    for (k, v) in e.fields.iter().filter(|(k, _)| *k != "message") {
        match v {
            Value::String(s) => {
                let _ = write!(line, " {k}={s}");
            }
            other => {
                let _ = write!(line, " {k}={other}");
            }
        }
    }
    if let Some(t) = e.trace_id {
        let _ = write!(line, " trace={t}");
    }
    line
}

/// Builds [`LogEvent`]s from `tracing` data and sends them to the file, console and
/// broadcast sinks. Every sink sees the redacted event.
pub struct NebulaLayer {
    redactor: Arc<Redactor>,
    file: Option<Mutex<DailyJsonl>>,
    console: bool,
    broadcast: broadcast::Sender<LogEvent>,
}

impl NebulaLayer {
    pub(crate) fn new(
        redactor: Arc<Redactor>,
        file: Option<DailyJsonl>,
        console: bool,
        broadcast: broadcast::Sender<LogEvent>,
    ) -> Self {
        Self {
            redactor,
            file: file.map(Mutex::new),
            console,
            broadcast,
        }
    }

    fn apply_fields(ctx: &mut SpanCtx, fields: &mut Map<String, Value>) {
        if let Some(t) = take_string(fields, TRACE_ID_FIELD).and_then(|s| s.parse().ok()) {
            ctx.trace_id = Some(t);
        }
        if let Some(t) = take_string(fields, TASK_ID_FIELD) {
            ctx.task_id = Some(t);
        }
        if let Some(s) = take_string(fields, STEP_ID_FIELD) {
            ctx.step_id = Some(s);
        }
    }

    fn emit(&self, event: &LogEvent) {
        // Logging must never take the process down; failed writes are dropped.
        if let Some(file) = &self.file
            && let Ok(line) = serde_json::to_string(event)
        {
            let mut w = file.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = w.write_line(event.ts, &line);
        }
        if self.console {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr().lock(), "{}", console_line(event));
        }
        // No receivers is the normal case when nobody runs `nebula logs tail`.
        let _ = self.broadcast.send(event.clone());
    }
}

impl<S> Layer<S> for NebulaLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let parent = span
            .parent()
            .and_then(|p| p.extensions().get::<SpanCtx>().cloned());
        let mut sc = SpanCtx {
            span_id: SpanId::new(),
            parent_span_id: parent.as_ref().map(|p| p.span_id),
            trace_id: parent.as_ref().and_then(|p| p.trace_id),
            task_id: parent.as_ref().and_then(|p| p.task_id.clone()),
            step_id: parent.and_then(|p| p.step_id),
        };
        let mut v = FieldVisitor::default();
        attrs.record(&mut v);
        Self::apply_fields(&mut sc, &mut v.fields);
        span.extensions_mut().insert(sc);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut v = FieldVisitor::default();
        values.record(&mut v);
        let mut ext = span.extensions_mut();
        if let Some(sc) = ext.get_mut::<SpanCtx>() {
            Self::apply_fields(sc, &mut v.fields);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut v = FieldVisitor::default();
        event.record(&mut v);
        let mut fields = v.fields;
        let name = take_string(&mut fields, EVENT_FIELD).unwrap_or_else(|| DEFAULT_EVENT.into());
        let sc = ctx
            .event_span(event)
            .and_then(|s| s.extensions().get::<SpanCtx>().cloned());
        let mut task_id = take_string(&mut fields, TASK_ID_FIELD);
        let mut step_id = take_string(&mut fields, STEP_ID_FIELD);
        let mut trace_id = take_string(&mut fields, TRACE_ID_FIELD).and_then(|s| s.parse().ok());
        let (span_id, parent_span_id) = match sc {
            Some(sc) => {
                trace_id = trace_id.or(sc.trace_id);
                task_id = task_id.or(sc.task_id);
                step_id = step_id.or(sc.step_id);
                (Some(sc.span_id), sc.parent_span_id)
            }
            None => (None, None),
        };
        let mut fields_value = Value::Object(fields);
        self.redactor.redact_value(&mut fields_value);
        let Value::Object(fields) = fields_value else {
            return;
        };
        let log = LogEvent {
            ts: OffsetDateTime::now_utc(),
            level: level_of(*meta.level()),
            target: meta.target().to_owned(),
            event: name,
            trace_id,
            span_id,
            parent_span_id,
            task_id,
            step_id,
            fields,
        };
        self.emit(&log);
    }
}
