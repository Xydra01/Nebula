#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use nebula_config::NebulaConfig;
use nebula_daemon::client::{Client, ClientError};
use nebula_daemon::{Daemon, DaemonError, Deps};
use nebula_model::SupervisorConfig;
use nebula_model::testing::{FakeLauncher, FakeState, content, finish, reasoning};
use nebula_proto::{
    ChatCancelParams, ChatId, ChatMessage, ChatStartParams, ChatStarted, CheckStatus, DaemonStatus,
    DoctorCheck, DoctorReport, Empty, Event, LogsSubscribeParams, Message, Method,
    ModelSetProfileParams, ModelState, ModelStatus, ResourceSnapshot, Role, error_code,
};
use nebula_resources::Sources;
use nebula_resources::sources::{GpuReading, GpuSource, SystemReading, SystemSource};
use nebula_telemetry::{Telemetry, TelemetryConfig};

const TIMEOUT: Duration = Duration::from_secs(10);

fn telemetry() -> Telemetry {
    static T: OnceLock<Telemetry> = OnceLock::new();
    T.get_or_init(|| nebula_telemetry::init(&TelemetryConfig::default()).unwrap())
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
    daemon: Daemon,
    launcher: Arc<FakeLauncher>,
    state: Arc<FakeState>,
    pipe: String,
    config: NebulaConfig,
    instance: String,
}

fn config(pipe_name: &str) -> NebulaConfig {
    let mut cfg = NebulaConfig::from_toml(None).unwrap();
    cfg.model = nebula_model::testing::config();
    cfg.daemon.pipe_name = pipe_name.to_owned();
    cfg.daemon.load_on_start = "a".into();
    cfg.daemon.embedding_profile = String::new();
    cfg.resources.sample_interval_ms = 100;
    cfg
}

fn deps(launcher: &Arc<FakeLauncher>, instance: &str) -> Deps {
    Deps {
        telemetry: telemetry(),
        launcher: Arc::clone(launcher) as _,
        sources: Some(Sources {
            gpu: Some(Box::new(FakeGpu)),
            gpu_processes: None,
            system: Box::new(FakeSystem),
        }),
        preflight: None,
        supervisor: policy(),
        instance_name: instance.to_owned(),
        local_checks: Arc::new(|_| {
            DoctorReport::from_checks(vec![DoctorCheck {
                name: "local.fake".into(),
                status: CheckStatus::Ok,
                detail: "fine".into(),
            }])
        }),
        artifact_checks: false,
    }
}

fn start() -> Harness {
    let unique = ChatId::new().to_string();
    let pipe_name = format!("nebula-test-{unique}");
    let instance = format!(r"Local\NebulaDaemonTest-{unique}");
    let state = FakeState::new();
    state.set_chunks(vec![
        reasoning("hmm"),
        content("Hel"),
        content("lo"),
        finish("stop"),
    ]);
    let launcher = FakeLauncher::new(Arc::clone(&state));
    let config = config(&pipe_name);
    let daemon = nebula_daemon::start(config.clone(), deps(&launcher, &instance)).unwrap();
    Harness {
        daemon,
        launcher,
        state,
        pipe: config.daemon.pipe_path(),
        config,
        instance,
    }
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
}

fn chat_params(text: &str) -> ChatStartParams {
    ChatStartParams {
        messages: vec![ChatMessage {
            role: Role::User,
            content: text.into(),
        }],
        profile: None,
        response_schema: None,
        max_tokens: None,
        reasoning: None,
    }
}

/// Reads events for `chat_id` until it ends; returns (text, reasoning, final event).
async fn collect_chat(c: &mut Client, chat_id: ChatId) -> (String, String, Event) {
    let (mut text, mut reasoning) = (String::new(), String::new());
    tokio::time::timeout(TIMEOUT, async {
        loop {
            match c.next_event().await.unwrap() {
                Event::ChatToken(t) if t.chat_id == chat_id => {
                    text.push_str(&t.text);
                    reasoning.push_str(&t.reasoning);
                }
                Event::ChatDone(d) if d.chat_id == chat_id => return Event::ChatDone(d),
                Event::ChatError(e) if e.chat_id == chat_id => return Event::ChatError(e),
                Event::ChatToken(t) => panic!("another chat's token: {t:?}"),
                Event::ChatDone(d) => panic!("another chat's done: {d:?}"),
                _ => {}
            }
        }
    })
    .await
    .map(|e| (text.clone(), reasoning.clone(), e))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn protocol_round_trip() {
    let h = start();
    let mut c = h.ready_client().await;

    let status: DaemonStatus = c.call(Method::DaemonStatus(Empty {})).await.unwrap();
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.model.profile, "a");

    let started: ChatStarted = c.call(Method::ChatStart(chat_params("hi"))).await.unwrap();
    let (text, reasoning, done) = collect_chat(&mut c, started.chat_id).await;
    assert_eq!((text.as_str(), reasoning.as_str()), ("Hello", "hmm"));
    let Event::ChatDone(done) = done else {
        panic!("{done:?}")
    };
    assert_eq!(done.usage.predicted_n, 3);
    let req = &h.state.requests("/v1/chat/completions")[0];
    assert_eq!(req["max_tokens"], 4096);

    let err = c
        .call::<Empty>(Method::ChatCancel(ChatCancelParams {
            chat_id: ChatId::new(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.rpc_code(), Some(error_code::NOT_FOUND));

    let s: ModelStatus = c
        .call(Method::ModelSetProfile(ModelSetProfileParams {
            profile: "b".into(),
        }))
        .await
        .unwrap();
    assert_eq!((s.profile.as_str(), s.state), ("b", ModelState::Ready));
    let err = c
        .call::<ModelStatus>(Method::ModelSetProfile(ModelSetProfileParams {
            profile: "nope".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.rpc_code(), Some(error_code::NOT_FOUND));

    // A chat naming another profile switches first.
    let mut p = chat_params("again");
    p.profile = Some("a".into());
    let started: ChatStarted = c.call(Method::ChatStart(p)).await.unwrap();
    let (text, _, _) = collect_chat(&mut c, started.chat_id).await;
    assert_eq!(text, "Hello");
    let s: ModelStatus = c.call(Method::ModelStatus(Empty {})).await.unwrap();
    assert_eq!(s.profile, "a");

    let snap: ResourceSnapshot = tokio::time::timeout(TIMEOUT, async {
        loop {
            match c.call(Method::ResourcesSnapshot(Empty {})).await {
                Ok(s) => return s,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(snap.vram_total_mib, 12_282);
    assert_eq!(snap.gpu_processes[0].name, "llama-server.exe");

    let report: DoctorReport = c.call(Method::DoctorRun(Empty {})).await.unwrap();
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["model.chat", "local.fake"]);
    assert_eq!(report.overall, CheckStatus::Ok);

    h.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn state_changes_and_logs_reach_clients() {
    let h = start();
    let mut watcher = h.ready_client().await;
    let _: Empty = watcher
        .call(Method::LogsSubscribe(LogsSubscribeParams {
            target_prefix: Some("nebula_model".into()),
            ..LogsSubscribeParams::default()
        }))
        .await
        .unwrap();
    let mut other = h.client().await;
    let _: ModelStatus = other
        .call(Method::ModelSetProfile(ModelSetProfileParams {
            profile: "b".into(),
        }))
        .await
        .unwrap();
    let (mut saw_state, mut saw_log) = (false, false);
    tokio::time::timeout(TIMEOUT, async {
        while !(saw_state && saw_log) {
            match watcher.next_event().await.unwrap() {
                Event::ModelStateChanged(e) if e.profile == "b" && e.to == ModelState::Ready => {
                    saw_state = true;
                }
                Event::LogEvent(e) => {
                    assert!(e.target.starts_with("nebula_model"), "{}", e.target);
                    saw_log = true;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    h.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_chat_concurrently() {
    let h = start();
    h.state.set_chunk_delay(Duration::from_millis(20));
    let mut a = h.ready_client().await;
    let mut b = h.client().await;
    let sa: ChatStarted = a.call(Method::ChatStart(chat_params("a"))).await.unwrap();
    let sb: ChatStarted = b.call(Method::ChatStart(chat_params("b"))).await.unwrap();
    assert_ne!(sa.chat_id, sb.chat_id);
    let (ra, rb) = tokio::join!(
        collect_chat(&mut a, sa.chat_id),
        collect_chat(&mut b, sb.chat_id)
    );
    assert_eq!(ra.0, "Hello");
    assert_eq!(rb.0, "Hello");
    let (Event::ChatDone(da), Event::ChatDone(db)) = (ra.2, rb.2) else {
        panic!("not done")
    };
    assert_eq!((da.chat_id, db.chat_id), (sa.chat_id, sb.chat_id));
    h.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnect_mid_stream_cancels_the_chat() {
    let h = start();
    h.state.set_chunks(
        (0..200)
            .map(|i| content(&format!("t{i} ")))
            .chain([finish("stop")])
            .collect(),
    );
    h.state.set_chunk_delay(Duration::from_millis(20));
    let mut a = h.ready_client().await;
    let started: ChatStarted = a
        .call(Method::ChatStart(chat_params("long")))
        .await
        .unwrap();
    loop {
        if matches!(a.next_event().await.unwrap(), Event::ChatToken(_)) {
            break;
        }
    }
    drop(a);

    let mut b = h.client().await;
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let r = b
                .call::<Empty>(Method::ChatCancel(ChatCancelParams {
                    chat_id: started.chat_id,
                }))
                .await;
            if r.is_err_and(|e| e.rpc_code() == Some(error_code::NOT_FOUND)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // The request is released just after the chat leaves the registry.
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let s: DaemonStatus = b.call(Method::DaemonStatus(Empty {})).await.unwrap();
            if s.model.state == ModelState::Ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    h.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_stops_a_running_chat() {
    let h = start();
    h.state.set_chunks(
        (0..200)
            .map(|i| content(&format!("t{i} ")))
            .chain([finish("stop")])
            .collect(),
    );
    h.state.set_chunk_delay(Duration::from_millis(20));
    let mut a = h.ready_client().await;
    let started: ChatStarted = a
        .call(Method::ChatStart(chat_params("long")))
        .await
        .unwrap();
    let _: Empty = a
        .call(Method::ChatCancel(ChatCancelParams {
            chat_id: started.chat_id,
        }))
        .await
        .unwrap();
    let (_, _, end) = collect_chat(&mut a, started.chat_id).await;
    let Event::ChatError(e) = end else {
        panic!("{end:?}")
    };
    assert_eq!(e.code, error_code::CANCELLED);
    h.daemon.shutdown().await;
}

/// The next response, skipping events (model state changes reach every client).
async fn next_response(c: &mut Client) -> nebula_proto::Response {
    loop {
        match c.read_message().await.unwrap() {
            Message::Response(r) => return r,
            Message::Notification(_) => {}
            Message::Request(r) => panic!("not a response: {r:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_lines_get_error_responses() {
    let h = start();
    let mut c = h.client().await;
    c.send_line(
        r#"{"jsonrpc":"2.0","id":7,"proto_version":99,"method":"daemon.status","params":{}}"#,
    )
    .await
    .unwrap();
    let r = next_response(&mut c).await;
    assert_eq!(r.id, 7.into());
    let err = r.into_result::<Empty>().unwrap_err();
    assert_eq!(
        ClientError::from(err).rpc_code(),
        Some(error_code::VERSION_MISMATCH)
    );

    c.send_line("not json").await.unwrap();
    let r = next_response(&mut c).await;
    assert_eq!(
        ClientError::from(r.into_result::<Empty>().unwrap_err()).rpc_code(),
        Some(error_code::PARSE_ERROR)
    );

    c.send_line(r#"{"jsonrpc":"2.0","id":"x","proto_version":1,"method":"nope","params":{}}"#)
        .await
        .unwrap();
    let r = next_response(&mut c).await;
    assert_eq!(r.id, nebula_proto::RequestId::Str("x".into()));
    assert_eq!(
        ClientError::from(r.into_result::<Empty>().unwrap_err()).rpc_code(),
        Some(error_code::METHOD_NOT_FOUND)
    );

    // The connection is still usable.
    let _: DaemonStatus = c.call(Method::DaemonStatus(Empty {})).await.unwrap();
    h.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_instance_is_refused() {
    let h = start();
    let launcher = FakeLauncher::new(FakeState::new());
    let err = nebula_daemon::start(h.config.clone(), deps(&launcher, &h.instance))
        .err()
        .unwrap();
    assert!(matches!(err, DaemonError::AlreadyRunning), "{err}");
    assert!(launcher.launches().is_empty());

    // A different mutex but the same pipe name: the pipe is already owned.
    let other = format!(r"Local\NebulaDaemonTest-{}", ChatId::new());
    let err = nebula_daemon::start(h.config.clone(), deps(&launcher, &other))
        .err()
        .unwrap();
    assert!(matches!(err, DaemonError::Pipe { .. }), "{err}");
    h.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_request_stops_everything() {
    let h = start();
    let mut c = h.ready_client().await;
    let _: Empty = c.call(Method::DaemonShutdown(Empty {})).await.unwrap();
    tokio::time::timeout(TIMEOUT, h.daemon.shutdown_requested())
        .await
        .unwrap();
    h.daemon.shutdown().await;
    assert!(
        h.launcher.current().unwrap().exited(),
        "model server still running"
    );
    let end = loop {
        match c.read_message().await {
            Ok(Message::Notification(_)) => {}
            other => break other,
        }
    };
    assert!(matches!(end, Err(ClientError::Closed)), "{end:?}");
    let err = Client::connect(&h.pipe, Duration::from_millis(200))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, ClientError::NotRunning(_)), "{err}");

    // The instance lock is released: a new daemon can start with the same names.
    let launcher = FakeLauncher::new(FakeState::new());
    let again = nebula_daemon::start(h.config.clone(), deps(&launcher, &h.instance)).unwrap();
    again.shutdown().await;
}
