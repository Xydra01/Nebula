#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use nebula_config::NebulaConfig;
use nebula_daemon::client::Client;
use nebula_daemon::game_mode::MockForegroundDetector;
use nebula_daemon::win::ForegroundWindowInfo;
use nebula_daemon::{Daemon, Deps};
use nebula_model::SupervisorConfig;
use nebula_model::testing::{FakeLauncher, FakeState, content, finish, reasoning};
use nebula_proto::{
    ChatId, CheckStatus, DoctorCheck, DoctorReport, Empty, Event, GameModeSetParams,
    GameModeStatusResponse, Method, ModelState, ModelStatus,
};
use nebula_telemetry::Telemetry;
use nebula_tools::ToolHost;

const TIMEOUT: Duration = Duration::from_secs(10);

fn telemetry() -> Telemetry {
    use std::sync::OnceLock;
    static T: OnceLock<Telemetry> = OnceLock::new();
    T.get_or_init(|| nebula_telemetry::init(&nebula_telemetry::TelemetryConfig::default()).unwrap())
        .clone()
}

fn policy() -> SupervisorConfig {
    SupervisorConfig {
        health_interval: Duration::from_millis(50),
        startup_timeout: Duration::from_secs(5),
        hang_threshold: 3,
        backoff_base: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        failure_window: Duration::from_secs(60),
        max_failures: 5,
        drain_timeout: Duration::from_millis(500),
    }
}

struct Harness {
    daemon: Daemon,
    pipe: String,
    mock_det: Arc<MockForegroundDetector>,
}

impl Harness {
    async fn client(&self) -> Client {
        Client::connect(&self.pipe, TIMEOUT).await.unwrap()
    }

    async fn ready_client(&self) -> Client {
        let mut c = self.client().await;
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let s: ModelStatus = c.call(Method::ModelStatus(Empty {})).await.unwrap();
                if s.state == ModelState::Ready {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        c
    }

    async fn shutdown(self) {
        self.daemon.shutdown().await;
    }
}

async fn start_harness(poll_interval_s: u64, cooldown_secs: u64) -> Harness {
    let unique = ChatId::new().to_string();
    let pipe_name = format!("nebula-gm-test-{unique}");
    let instance = format!(r"Local\NebulaGmTest-{unique}");

    let mut cfg = NebulaConfig::from_toml(None).unwrap();
    cfg.model = nebula_model::testing::config();
    cfg.daemon.pipe_name = pipe_name;
    cfg.daemon.load_on_start = "a".into();
    cfg.daemon.embedding_profile = String::new();
    cfg.resources.sample_interval_ms = 50;
    cfg.game_mode.enabled = true;
    cfg.game_mode.poll_interval_s = poll_interval_s;
    cfg.game_mode.cooldown_secs = cooldown_secs;

    let state = FakeState::new();
    state.set_chunks(vec![reasoning("hmm"), content("Hello"), finish("stop")]);
    let launcher = FakeLauncher::new(Arc::clone(&state));
    let mock_det = Arc::new(MockForegroundDetector::new());

    let deps = Deps {
        telemetry: telemetry(),
        launcher: launcher as _,
        tool_host: ToolHost::empty(),
        sources: None,
        preflight: None,
        supervisor: policy(),
        instance_name: instance,
        local_checks: Arc::new(|_| {
            DoctorReport::from_checks(vec![DoctorCheck {
                name: "local.fake".into(),
                status: CheckStatus::Ok,
                detail: "fine".into(),
            }])
        }),
        artifact_checks: false,
        foreground_detector: Some(Arc::clone(&mock_det) as _),
    };

    let pipe_path = cfg.daemon.pipe_path();
    let daemon = nebula_daemon::start(cfg, deps).unwrap();

    Harness {
        daemon,
        pipe: pipe_path,
        mock_det,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_game_mode_manual_override_unloads_and_reloads() {
    let h = start_harness(10, 10).await;
    let mut c = h.ready_client().await;

    // Initially inactive
    let status: GameModeStatusResponse = c.call(Method::GameModeStatus(Empty {})).await.unwrap();
    assert!(!status.active);
    assert_eq!(status.manual_override, None);

    // Turn ON manually
    let res: GameModeStatusResponse = c
        .call(Method::GameModeSet(GameModeSetParams { enabled: true }))
        .await
        .unwrap();
    assert!(res.active);
    assert_eq!(res.manual_override, Some(true));

    // Verify model is unloaded
    let model_status: ModelStatus = c.call(Method::ModelStatus(Empty {})).await.unwrap();
    assert_eq!(model_status.state, ModelState::Unloaded);

    // Turn OFF manually
    let res: GameModeStatusResponse = c
        .call(Method::GameModeSet(GameModeSetParams { enabled: false }))
        .await
        .unwrap();
    assert!(!res.active);
    assert_eq!(res.manual_override, Some(false));

    // Verify model is reloaded
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let ms: ModelStatus = c.call(Method::ModelStatus(Empty {})).await.unwrap();
            if ms.state == ModelState::Ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("model failed to reload");

    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_game_mode_event_broadcast() {
    let h = start_harness(10, 10).await;
    let mut c = h.ready_client().await;
    let mut sub_client = h.client().await;

    // Turn ON manually
    let _: GameModeStatusResponse = c
        .call(Method::GameModeSet(GameModeSetParams { enabled: true }))
        .await
        .unwrap();

    // Read event on sub_client
    let event = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Event::GameModeChanged(gm) = sub_client.next_event().await.unwrap() {
                return gm;
            }
        }
    })
    .await
    .expect("did not receive GameModeChanged event");

    assert!(event.active);
    assert!(event.reason.unwrap().contains("manual override"));

    // Turn OFF manually
    let _: GameModeStatusResponse = c
        .call(Method::GameModeSet(GameModeSetParams { enabled: false }))
        .await
        .unwrap();

    let event = tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Event::GameModeChanged(gm) = sub_client.next_event().await.unwrap() {
                return gm;
            }
        }
    })
    .await
    .expect("did not receive GameModeChanged off event");

    assert!(!event.active);

    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_game_mode_detection_loop_and_cooldown() {
    // 1-second poll interval, 1-second cooldown
    let h = start_harness(1, 1).await;
    let mut c = h.ready_client().await;

    // Simulate game foreground window
    h.mock_det.set(Some(ForegroundWindowInfo {
        pid: 7777,
        is_fullscreen: true,
    }));

    // Wait for game mode to become active
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let status: GameModeStatusResponse =
                c.call(Method::GameModeStatus(Empty {})).await.unwrap();
            if status.active {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("game mode never activated via detector");

    // Verify model is unloaded
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let model_status: ModelStatus = c.call(Method::ModelStatus(Empty {})).await.unwrap();
            if model_status.state == ModelState::Unloaded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("model never unloaded after game mode activated");

    // Clear game window (game exited)
    h.mock_det.set(None);

    // Wait for cooldown + deactivation
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let status: GameModeStatusResponse =
                c.call(Method::GameModeStatus(Empty {})).await.unwrap();
            if !status.active {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("game mode never deactivated after game exited");

    // Verify model is reloaded
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let ms: ModelStatus = c.call(Method::ModelStatus(Empty {})).await.unwrap();
            if ms.state == ModelState::Ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("model failed to reload after game exited");

    h.shutdown().await;
}
