#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::fs;
use std::path::Path;

use nebula_proto::{LogEvent, LogLevel, TraceId};
use nebula_telemetry::budget::{self, Confirmation};
use nebula_telemetry::redact::MASK;
use nebula_telemetry::writer::file_name_for;
use nebula_telemetry::{TelemetryConfig, build};
use time::macros::date;

// Built at runtime so no scanner ever sees a token-shaped literal in the source.
fn planted_token() -> String {
    format!("ghp_{}", "Z9".repeat(18))
}

fn config(dir: &Path) -> TelemetryConfig {
    TelemetryConfig {
        log_dir: Some(dir.to_path_buf()),
        filter: "trace".into(),
        ..TelemetryConfig::default()
    }
}

fn read_events(dir: &Path) -> Vec<LogEvent> {
    let mut events = Vec::new();
    for (_, path, _) in budget::daily_logs(dir).unwrap() {
        for line in fs::read_to_string(path).unwrap().lines() {
            events.push(serde_json::from_str(line).unwrap());
        }
    }
    events
}

#[test]
fn events_are_jsonl_with_trace_and_span_ids() {
    let dir = tempfile::tempdir().unwrap();
    let (_t, sub) = build(&config(dir.path())).unwrap();
    let trace = TraceId::new();
    tracing::subscriber::with_default(sub, || {
        let outer = tracing::info_span!("chat", trace_id = %trace, task_id = "t1");
        let _o = outer.enter();
        tracing::info!(event = "chat.start", profile = "standard");
        let inner = tracing::info_span!("model", step_id = "s1");
        let _i = inner.enter();
        tracing::warn!(event = "model.call", prompt_n = 42_u64, "slow");
    });
    tracing::subscriber::with_default(build(&config(dir.path())).unwrap().1, || {
        tracing::debug!("no span");
    });

    let events = read_events(dir.path());
    assert_eq!(events.len(), 3);
    let (start, call, bare) = (&events[0], &events[1], &events[2]);

    assert_eq!(start.event, "chat.start");
    assert_eq!(start.level, LogLevel::Info);
    assert_eq!(start.trace_id, Some(trace));
    assert_eq!(start.task_id.as_deref(), Some("t1"));
    assert!(start.span_id.is_some());
    assert_eq!(start.parent_span_id, None);
    assert_eq!(start.fields["profile"], "standard");
    assert!(start.target.contains("telemetry"));

    assert_eq!(call.event, "model.call");
    assert_eq!(call.trace_id, Some(trace));
    assert_eq!(call.task_id.as_deref(), Some("t1"));
    assert_eq!(call.step_id.as_deref(), Some("s1"));
    assert_eq!(call.parent_span_id, start.span_id);
    assert_ne!(call.span_id, start.span_id);
    assert_eq!(call.fields["prompt_n"], 42);
    assert_eq!(call.fields["message"], "slow");

    assert_eq!(bare.event, "log");
    assert_eq!(bare.trace_id, None);
    assert_eq!(bare.span_id, None);
}

#[test]
fn trace_id_recorded_after_span_creation_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let (_t, sub) = build(&config(dir.path())).unwrap();
    let trace = TraceId::new();
    tracing::subscriber::with_default(sub, || {
        let span = tracing::info_span!("chat", trace_id = tracing::field::Empty);
        span.record("trace_id", tracing::field::display(trace));
        let _g = span.enter();
        tracing::info!(event = "x");
    });
    assert_eq!(read_events(dir.path())[0].trace_id, Some(trace));
}

#[test]
fn filter_drops_events_below_level() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = TelemetryConfig {
        filter: "warn".into(),
        ..config(dir.path())
    };
    let (_t, sub) = build(&cfg).unwrap();
    tracing::subscriber::with_default(sub, || {
        tracing::info!(event = "dropped");
        tracing::error!(event = "kept");
    });
    let events = read_events(dir.path());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "kept");
}

#[test]
fn bad_filter_is_an_error() {
    let cfg = TelemetryConfig {
        filter: "info,=[".into(),
        ..TelemetryConfig::default()
    };
    assert!(build(&cfg).is_err());
}

#[test]
fn redaction_masks_tokens_and_registered_secrets_in_fields() {
    let dir = tempfile::tempdir().unwrap();
    let (t, sub) = build(&config(dir.path())).unwrap();
    assert!(t.redactor().register("hunter2-but-longer"));
    assert!(!t.redactor().register("short"));
    let token = planted_token();
    tracing::subscriber::with_default(sub, || {
        tracing::info!(
            event = "leak",
            header = format!("Authorization: Bearer {}", "a1B2".repeat(8)).as_str(),
            stderr = %format!("push failed for {token} at remote"),
            "password is hunter2-but-longer"
        );
    });
    let raw: String = budget::daily_logs(dir.path())
        .unwrap()
        .into_iter()
        .map(|(_, p, _)| fs::read_to_string(p).unwrap())
        .collect();
    assert!(!raw.contains(&token), "{raw}");
    assert!(!raw.contains("hunter2-but-longer"), "{raw}");
    assert!(!raw.contains("a1B2a1B2"), "{raw}");
    let e = &read_events(dir.path())[0];
    assert_eq!(
        e.fields["stderr"],
        format!("push failed for {MASK} at remote")
    );
    assert_eq!(e.fields["message"], format!("password is {MASK}"));
    assert_eq!(e.fields["header"], format!("Authorization: {MASK}"));
}

#[test]
fn blobs_dedupe_round_trip_and_are_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let (t, _sub) = build(&config(dir.path())).unwrap();
    let blobs = t.blobs().unwrap();

    let a = blobs.put(b"the full prompt").unwrap();
    let b = blobs.put(b"the full prompt").unwrap();
    assert_eq!(a, b);
    assert_eq!(blobs.get(&a).unwrap(), b"the full prompt");
    let path = blobs.path_of(&a);
    assert!(path.exists());
    assert_eq!(path.parent().unwrap().file_name().unwrap(), &a.hex()[..2]);
    assert_eq!(t.usage().unwrap().blob_files, 1);

    let r: nebula_telemetry::BlobRef = a.to_string().parse().unwrap();
    assert_eq!(r, a);
    assert!("sha256:xyz".parse::<nebula_telemetry::BlobRef>().is_err());

    let token = planted_token();
    let leaked = blobs.put(format!("output {token} end").as_bytes()).unwrap();
    let stored = String::from_utf8(blobs.get(&leaked).unwrap()).unwrap();
    assert_eq!(stored, format!("output {MASK} end"));
    let on_disk = zstd::decode_all(&fs::read(blobs.path_of(&leaked)).unwrap()[..]).unwrap();
    assert!(!String::from_utf8_lossy(&on_disk).contains(&token));
}

#[test]
fn corrupt_blob_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let (t, _sub) = build(&config(dir.path())).unwrap();
    let blobs = t.blobs().unwrap();
    let r = blobs.put(b"payload").unwrap();
    fs::write(
        blobs.path_of(&r),
        zstd::encode_all(&b"tampered"[..], 3).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        blobs.get(&r),
        Err(nebula_telemetry::BlobError::Corrupt(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn broadcast_delivers_redacted_events() {
    let (t, sub) = build(&TelemetryConfig {
        filter: "trace".into(),
        ..TelemetryConfig::default()
    })
    .unwrap();
    let mut rx = t.subscribe();
    let token = planted_token();
    tracing::subscriber::with_default(sub, || {
        tracing::info!(event = "tail.me", value = token.as_str());
    });
    let e = rx.recv().await.unwrap();
    assert_eq!(e.event, "tail.me");
    assert_eq!(e.fields["value"], MASK);
}

#[test]
fn old_logs_move_to_archive_and_today_stays() {
    let hot = tempfile::tempdir().unwrap();
    let cold = tempfile::tempdir().unwrap();
    let today = date!(2026 - 10 - 20);
    for d in [
        date!(2026 - 10 - 01),
        date!(2026 - 10 - 12),
        date!(2026 - 10 - 13),
        today,
    ] {
        fs::write(hot.path().join(file_name_for(d)), "{}\n").unwrap();
    }
    fs::write(hot.path().join("notes.txt"), "not a log").unwrap();

    let report = budget::archive_old_logs(hot.path(), cold.path(), today, 7).unwrap();
    assert_eq!(report.moved.len(), 2);
    assert_eq!(report.bytes, 6);
    let left: Vec<_> = budget::daily_logs(hot.path())
        .unwrap()
        .into_iter()
        .map(|(d, _, _)| d)
        .collect();
    assert_eq!(left, [date!(2026 - 10 - 13), today]);
    assert_eq!(budget::daily_logs(cold.path()).unwrap().len(), 2);
    assert!(hot.path().join("notes.txt").exists());
}

#[test]
fn archive_conflicts_are_left_in_place() {
    let hot = tempfile::tempdir().unwrap();
    let cold = tempfile::tempdir().unwrap();
    let old = file_name_for(date!(2026 - 01 - 01));
    fs::write(hot.path().join(&old), "hot").unwrap();
    fs::write(cold.path().join(&old), "cold").unwrap();
    let report =
        budget::archive_old_logs(hot.path(), cold.path(), date!(2026 - 10 - 20), 7).unwrap();
    assert!(report.moved.is_empty());
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(fs::read_to_string(hot.path().join(&old)).unwrap(), "hot");
    assert_eq!(fs::read_to_string(cold.path().join(&old)).unwrap(), "cold");
}

#[test]
fn archive_cap_deletes_oldest_days_without_approval_but_keeps_newest_and_other_files() {
    let cold = tempfile::tempdir().unwrap();
    let days: Vec<_> = (1..=5)
        .map(|d| time::Date::from_calendar_date(2026, time::Month::January, d).unwrap())
        .collect();
    for d in &days {
        fs::write(cold.path().join(file_name_for(*d)), vec![b'x'; 100]).unwrap();
    }
    fs::write(cold.path().join("keep-me.txt"), vec![b'y'; 10_000]).unwrap();

    let pruned = budget::enforce_archive_cap(cold.path(), 250).unwrap();
    assert_eq!(pruned.files.len(), 3);
    let left: Vec<_> = budget::daily_logs(cold.path())
        .unwrap()
        .into_iter()
        .map(|(d, _, _)| d)
        .collect();
    assert_eq!(left, days[3..]);
    assert!(cold.path().join("keep-me.txt").exists());

    // Even a cap of zero keeps the newest day.
    budget::enforce_archive_cap(cold.path(), 0).unwrap();
    assert_eq!(budget::daily_logs(cold.path()).unwrap().len(), 1);
    assert!(
        budget::enforce_archive_cap(cold.path(), 0)
            .unwrap()
            .files
            .is_empty()
    );
}

#[test]
fn prune_plans_oldest_first_and_large_plans_need_approval() {
    let cold = tempfile::tempdir().unwrap();
    for (i, d) in [
        date!(2026 - 01 - 01),
        date!(2026 - 01 - 02),
        date!(2026 - 01 - 03),
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(
            cold.path().join(file_name_for(d)),
            vec![b'x'; 100 * (i + 1)],
        )
        .unwrap();
    }
    let plan = budget::plan_archive_prune(cold.path(), 350).unwrap();
    assert_eq!(plan.archive_bytes, 600);
    assert_eq!(plan.files.len(), 2);
    assert_eq!(plan.bytes, 300);
    assert!(!plan.needs_confirmation());
    budget::execute_prune(&plan, Confirmation::None).unwrap();
    assert_eq!(budget::daily_logs(cold.path()).unwrap().len(), 1);

    let big = budget::PrunePlan {
        files: vec![(cold.path().join("whatever"), budget::CONFIRM_BYTES + 1)],
        bytes: budget::CONFIRM_BYTES + 1,
        archive_bytes: 0,
    };
    assert!(big.needs_confirmation());
    assert!(matches!(
        budget::execute_prune(&big, Confirmation::None),
        Err(budget::BudgetError::NeedsConfirmation { .. })
    ));
}
