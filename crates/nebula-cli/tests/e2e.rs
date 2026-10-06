#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

//! Drives the CLI's commands against an in-process daemon with a fake llama-server.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use nebula_cli::{ChatArgs, Command, Ctx, DaemonAction, ModelAction, Style};
use nebula_config::NebulaConfig;
use nebula_daemon::{Daemon, Deps};
use nebula_model::SupervisorConfig;
use nebula_model::testing::{FakeLauncher, FakeState, content, finish, reasoning};
use nebula_proto::{ChatId, CheckStatus, DoctorCheck, DoctorReport};
use nebula_resources::Sources;
use nebula_resources::sources::{GpuReading, GpuSource, SystemReading, SystemSource};
use nebula_telemetry::{Telemetry, TelemetryConfig};
use nebula_tools::ToolHost;

fn telemetry() -> Telemetry {
    static T: OnceLock<Telemetry> = OnceLock::new();
    T.get_or_init(|| nebula_telemetry::init(&TelemetryConfig::default()).unwrap())
        .clone()
}

struct FakeGpu;
impl GpuSource for FakeGpu {
    fn read(&mut self) -> Result<GpuReading, nebula_resources::ResourceError> {
        Ok(GpuReading {
            name: "Fake GPU".into(),
            vram_used_mib: 9_000,
            vram_total_mib: 12_282,
            processes: vec![(4242, Some(8_000))],
            ..GpuReading::default()
        })
    }
}

struct FakeSystem;
impl SystemSource for FakeSystem {
    fn read(
        &mut self,
        _mounts: &[String],
    ) -> Result<SystemReading, nebula_resources::ResourceError> {
        Ok(SystemReading {
            ram_total_mib: 32_768,
            ..SystemReading::default()
        })
    }
    fn process_names(&mut self, pids: &[u32]) -> HashMap<u32, String> {
        pids.iter()
            .map(|p| (*p, "llama-server.exe".to_owned()))
            .collect()
    }
}

struct Harness {
    daemon: Option<Daemon>,
    state: Arc<FakeState>,
    ctx: Ctx,
}

fn start() -> Harness {
    let unique = ChatId::new().to_string();
    let mut config = NebulaConfig::from_toml(None).unwrap();
    config.model = nebula_model::testing::config();
    config.daemon.pipe_name = format!("nebula-cli-test-{unique}");
    config.daemon.load_on_start = "a".into();
    config.daemon.embedding_profile = String::new();
    config.resources.sample_interval_ms = 100;

    let state = FakeState::new();
    state.set_chunks(vec![
        reasoning("hmm"),
        content("Hel"),
        content("lo"),
        finish("stop"),
    ]);
    let launcher = FakeLauncher::new(Arc::clone(&state));
    let deps = Deps {
        telemetry: telemetry(),
        launcher: launcher as _,
        tool_host: Arc::new(ToolHost::empty()),
        sources: Some(Sources {
            gpu: Some(Box::new(FakeGpu)),
            gpu_processes: None,
            system: Box::new(FakeSystem),
        }),
        preflight: None,
        supervisor: SupervisorConfig {
            health_interval: Duration::from_millis(50),
            startup_timeout: Duration::from_secs(5),
            hang_threshold: 3,
            backoff_base: Duration::from_millis(50),
            backoff_max: Duration::from_millis(200),
            failure_window: Duration::from_secs(60),
            max_failures: 5,
            drain_timeout: Duration::from_millis(500),
        },
        instance_name: format!(r"Local\NebulaCliTest-{unique}"),
        local_checks: Arc::new(|_| {
            DoctorReport::from_checks(vec![DoctorCheck {
                name: "local.fake".into(),
                status: CheckStatus::Warn,
                detail: "low on something".into(),
            }])
        }),
        artifact_checks: false,
    };
    let daemon = nebula_daemon::start(config.clone(), deps).unwrap();
    let ctx = Ctx {
        pipe: config.daemon.pipe_path(),
        config,
        json: false,
        style: Style::PLAIN,
        interactive: false,
        daemon_exe: None,
    };
    Harness {
        daemon: Some(daemon),
        state,
        ctx,
    }
}

impl Harness {
    async fn run(&self, command: Command, input: &[u8]) -> (u8, String) {
        let mut out = Vec::new();
        let code = tokio::time::timeout(
            Duration::from_secs(20),
            nebula_cli::run(command, &self.ctx, input, &mut out),
        )
        .await
        .unwrap()
        .unwrap();
        (code, String::from_utf8(out).unwrap())
    }

    async fn stop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.shutdown().await;
        }
    }
}

fn chat() -> Command {
    Command::Chat(ChatArgs {
        profile: None,
        schema: None,
        reasoning: None,
        max_tokens: None,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_against_a_running_daemon() {
    let mut h = start();

    let (code, out) = h
        .run(
            Command::Daemon {
                action: DaemonAction::Start,
            },
            b"",
        )
        .await;
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("already running"), "{out}");

    let (code, out) = h
        .run(
            Command::Model {
                action: ModelAction::Status,
            },
            b"",
        )
        .await;
    assert_eq!(code, 0);
    assert!(out.starts_with("model    a "), "{out}");

    let (code, out) = h
        .run(chat(), b"hello\n\n/reset\nagain\nmore\n/exit\nignored\n")
        .await;
    assert_eq!(code, 0, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "Hello", "{out}");
    assert!(lines[1].starts_with("[prompt "), "{out}");
    assert_eq!(lines[2], "(history cleared)");
    assert_eq!(lines[3], "Hello");
    assert_eq!(lines[5], "Hello");
    assert_eq!(lines.len(), 7, "{out}");
    assert!(
        !out.contains("hmm"),
        "reasoning is hidden when not interactive: {out}"
    );

    let reqs = h.state.requests("/v1/chat/completions");
    assert_eq!(reqs.len(), 3);
    let roles = |i: usize| -> Vec<String> {
        reqs[i]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(roles(0), ["user"]);
    assert_eq!(roles(1), ["user"], "/reset clears history");
    assert_eq!(roles(2), ["user", "assistant", "user"]);
    assert_eq!(reqs[2]["messages"][1]["content"], "Hello");

    let (code, out) = h.run(Command::Resources, b"").await;
    assert_eq!(code, 0);
    assert!(out.contains("llama-server.exe"), "{out}");

    let (code, out) = h.run(Command::Doctor, b"").await;
    assert_eq!(code, 1, "the fake local check warns: {out}");
    assert!(
        out.contains("daemon") && out.contains("local.fake"),
        "{out}"
    );

    let (code, out) = h
        .run(
            Command::Model {
                action: ModelAction::Profile { name: "b".into() },
            },
            b"",
        )
        .await;
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("model    b ready"), "{out}");

    h.ctx.json = true;
    let (code, out) = h.run(chat(), b"hi\n").await;
    assert_eq!(code, 0);
    let turn: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(turn["text"], "Hello");
    assert_eq!(turn["reasoning"], "hmm");
    assert_eq!(turn["stop_reason"], "stop");
    h.ctx.json = false;

    let (code, out) = h
        .run(
            Command::Daemon {
                action: DaemonAction::Stop,
            },
            b"",
        )
        .await;
    assert_eq!(code, 0, "{out}");
    assert!(out.ends_with("stopped\n"), "{out}");
    let (code, out) = h
        .run(
            Command::Daemon {
                action: DaemonAction::Status,
            },
            b"",
        )
        .await;
    assert_eq!(code, 1);
    assert_eq!(out, "daemon   not running\n");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the real local checks (PowerShell, NVML, disks)"]
async fn doctor_without_a_daemon_fails_and_runs_local_checks() {
    let mut h = start();
    h.stop().await;
    let (code, out) = h.run(Command::Doctor, b"").await;
    assert_eq!(code, 2, "{out}");
    assert!(out.starts_with("FAIL  daemon"), "{out}");
    assert!(out.contains("overall: FAIL"), "{out}");
}
