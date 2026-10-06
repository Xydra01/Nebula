#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use clap::Parser;
use nebula_cli::format::{self, Style};
use nebula_cli::{
    BackupAction, ChatArgs, Cli, Command, DaemonAction, Level, LogsAction, ModelAction, Reasoning,
    doctor_exit_code,
};
use nebula_config::NebulaConfig;
use nebula_proto::{
    CheckStatus, DaemonStatus, DiskUsage, DoctorCheck, DoctorReport, GpuProcess, ModelState,
    ModelStatus, ResourceSnapshot, StopReason, Usage,
};
use time::macros::datetime;

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("nebula").chain(args.iter().copied())).unwrap()
}

#[test]
fn parses_every_command() {
    assert!(matches!(
        parse(&["daemon", "start"]).command,
        Command::Daemon {
            action: DaemonAction::Start
        }
    ));
    assert!(matches!(
        parse(&["daemon", "stop"]).command,
        Command::Daemon {
            action: DaemonAction::Stop
        }
    ));
    let cli = parse(&["daemon", "status", "--json"]);
    assert!(cli.json);
    assert!(matches!(
        cli.command,
        Command::Daemon {
            action: DaemonAction::Status
        }
    ));

    let Command::Chat(ChatArgs {
        profile,
        schema,
        reasoning,
        max_tokens,
    }) = parse(&[
        "--json",
        "chat",
        "--profile",
        "long",
        "--schema",
        "s.json",
        "--reasoning",
        "xhigh",
        "--max-tokens",
        "100",
    ])
    .command
    else {
        panic!("not chat")
    };
    assert_eq!(profile.as_deref(), Some("long"));
    assert_eq!(schema.unwrap().to_str(), Some("s.json"));
    assert_eq!(reasoning, Some(Reasoning::Xhigh));
    assert_eq!(max_tokens, Some(100));

    let Command::Model {
        action: ModelAction::Profile { name },
    } = parse(&["model", "profile", "standard"]).command
    else {
        panic!("not model profile")
    };
    assert_eq!(name, "standard");
    assert!(matches!(
        parse(&["model", "status"]).command,
        Command::Model {
            action: ModelAction::Status
        }
    ));

    let trace = "01J9ZQ3V5W8X0Y2Z4A6B8C0D2E";
    let Command::Logs {
        action:
            LogsAction::Tail {
                level,
                trace: t,
                target,
            },
    } = parse(&["logs", "tail", "--level", "warn", "--trace", trace]).command
    else {
        panic!("not logs tail")
    };
    assert_eq!(level, Some(Level::Warn));
    assert_eq!(t.unwrap().to_string(), trace);
    assert!(target.is_none());

    assert!(matches!(
        parse(&["backup", "reauth", "--record-only"]).command,
        Command::Backup {
            action: BackupAction::Reauth { record_only: true }
        }
    ));
    assert!(matches!(
        parse(&["backup", "reauth"]).command,
        Command::Backup {
            action: BackupAction::Reauth { record_only: false }
        }
    ));

    assert!(matches!(parse(&["resources"]).command, Command::Resources));
    assert!(matches!(parse(&["doctor"]).command, Command::Doctor));
}

#[test]
fn rejects_bad_arguments() {
    for args in [
        &["nebula"][..],
        &["nebula", "chat", "--reasoning", "high"],
        &["nebula", "logs", "tail", "--trace", "nope"],
        &["nebula", "model", "profile"],
        &["nebula", "daemon", "restart"],
    ] {
        assert!(Cli::try_parse_from(args).is_err(), "{args:?} should fail");
    }
}

#[test]
fn doctor_exit_codes() {
    assert_eq!(doctor_exit_code(CheckStatus::Ok), 0);
    assert_eq!(doctor_exit_code(CheckStatus::Warn), 1);
    assert_eq!(doctor_exit_code(CheckStatus::Fail), 2);
}

#[test]
fn style_plain_has_no_escapes() {
    assert_eq!(Style::PLAIN.bad("x"), "x");
    assert_eq!(Style { color: true }.bad("x"), "\x1b[31mx\x1b[0m");
}

fn model() -> ModelStatus {
    ModelStatus {
        profile: "standard".into(),
        state: ModelState::Ready,
        since: datetime!(2026-10-04 12:00:00 UTC),
        restarts: 1,
        last_error: Some("llama-server exited with code 1".into()),
    }
}

#[test]
fn snapshot_daemon_status() {
    let s = DaemonStatus {
        version: "0.1.0".into(),
        proto_version: 2,
        pid: 1234,
        uptime_s: 3725,
        model: model(),
    };
    insta::assert_snapshot!(format::daemon_status(
        &s,
        datetime!(2026-10-04 12:05:03 UTC),
        Style::PLAIN
    ));
}

#[test]
fn snapshot_resources() {
    let gb = 1024 * 1024 * 1024;
    let snap = ResourceSnapshot {
        taken_at: datetime!(2026-10-04 12:00:00 UTC),
        vram_used_mib: 9_300,
        vram_total_mib: 12_282,
        gpu_util_pct: Some(87),
        ram_used_mib: 20_480,
        ram_total_mib: 32_607,
        commit_used_mib: 37_000,
        commit_limit_mib: 65_375,
        cpu_pct: 12.4,
        disks: vec![
            DiskUsage {
                mount: r"F:\".into(),
                free_bytes: 700 * gb,
                total_bytes: 1863 * gb,
            },
            DiskUsage {
                mount: r"D:\".into(),
                free_bytes: 40 * gb,
                total_bytes: 931 * gb,
            },
        ],
        gpu_processes: vec![
            GpuProcess {
                pid: 4242,
                name: "llama-server.exe".into(),
                vram_mib: 8_900,
            },
            GpuProcess {
                pid: 77,
                name: "dwm.exe".into(),
                vram_mib: 210,
            },
        ],
    };
    let cfg = NebulaConfig::from_toml(None).unwrap();
    insta::assert_snapshot!(format::resources(
        &snap,
        &cfg.resources.volumes,
        Style::PLAIN
    ));
}

#[test]
fn snapshot_doctor() {
    let report = DoctorReport::from_checks(vec![
        DoctorCheck {
            name: "daemon".into(),
            status: CheckStatus::Ok,
            detail: "running".into(),
        },
        DoctorCheck {
            name: "disk.F:".into(),
            status: CheckStatus::Warn,
            detail: "90 GB free (warn < 100 GB)".into(),
        },
        DoctorCheck {
            name: "backup".into(),
            status: CheckStatus::Fail,
            detail: "never ran".into(),
        },
    ]);
    insta::assert_snapshot!(format::doctor(&report, Style::PLAIN));
}

#[test]
fn snapshot_usage_lines() {
    let u = Usage {
        prompt_n: 120,
        prompt_ms: 60.0,
        predicted_n: 50,
        predicted_ms: 1000.0,
    };
    let lines = [
        format::usage_line(&u, StopReason::Stop, Style::PLAIN),
        format::usage_line(&u, StopReason::Length, Style::PLAIN),
        format::usage_line(&Usage::default(), StopReason::Cancelled, Style::PLAIN),
    ];
    insta::assert_snapshot!(lines.join("\n"));
}

#[test]
fn durations() {
    assert_eq!(format::duration(42), "42s");
    assert_eq!(format::duration(303), "5m 03s");
    assert_eq!(format::duration(7380), "2h 03m");
    assert_eq!(format::duration(273_600), "3d 04h");
}
