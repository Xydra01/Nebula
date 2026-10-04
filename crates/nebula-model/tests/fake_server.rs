#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

mod fake;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fake::{FakeLauncher, FakeState, content, finish, reasoning};
use futures_util::StreamExt;
use nebula_model::{
    Activity, BackendHealth, ChatRequest, LlamaServerBackend, ModelBackend, ModelError,
    ModelManager, StreamItem, SupervisorConfig,
};
use nebula_proto::{ChatMessage, LogEvent, ModelState, ReasoningEffort, Role, StopReason};
use nebula_telemetry::{TelemetryConfig, budget};
use serde_json::json;

fn hello() -> ChatRequest {
    ChatRequest::new(vec![ChatMessage {
        role: Role::User,
        content: "hi".into(),
    }])
}

async fn backend(state: &Arc<FakeState>) -> LlamaServerBackend {
    *state.api_key.lock().unwrap() = Some("k3y".into());
    let (url, _task) = fake::start(Arc::clone(state)).await;
    LlamaServerBackend::new(
        url,
        Some("k3y".into()),
        "a",
        fake::profile("a.gguf"),
        None,
        Activity::new(),
    )
    .unwrap()
}

async fn idle(activity: &Activity) {
    for _ in 0..200 {
        if activity.in_flight() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("requests still in flight");
}

#[tokio::test]
async fn chat_streams_tokens_and_logs_model_call_with_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let (telemetry, sub) = nebula_telemetry::build(&TelemetryConfig {
        log_dir: Some(dir.path().into()),
        filter: "info".into(),
        ..TelemetryConfig::default()
    })
    .unwrap();
    let _sub = tracing::subscriber::set_default(sub);

    let state = FakeState::new();
    state.set_chunks(vec![
        reasoning("thinking"),
        content("Hel"),
        content("lo"),
        finish("stop"),
    ]);
    *state.api_key.lock().unwrap() = Some("k3y".into());
    let (url, _task) = fake::start(Arc::clone(&state)).await;
    let b = LlamaServerBackend::new(
        url,
        Some("k3y".into()),
        "a",
        fake::profile("a.gguf"),
        telemetry.blobs().cloned(),
        Activity::new(),
    )
    .unwrap();

    let mut req = hello();
    req.trace_id = Some(nebula_proto::TraceId::new());
    let out = b.chat(req.clone()).await.unwrap().collect().await.unwrap();
    assert_eq!(out.text, "Hello");
    assert_eq!(out.reasoning, "thinking");
    assert_eq!(out.stop_reason, Some(StopReason::Stop));
    let u = out.usage.unwrap();
    assert_eq!((u.prompt_n, u.predicted_n), (12, 3));
    assert!((u.prompt_ms - 34.5).abs() < f64::EPSILON);

    let body = &state.requests("/v1/chat/completions")[0];
    assert_eq!(body["stream"], true);
    assert_eq!(body["cache_prompt"], true);
    assert_eq!(body["reasoning_effort"], "none");
    assert_eq!(body["chat_template_kwargs"]["reasoning_effort"], "none");
    assert_eq!(body["temperature"], 0.7);
    assert_eq!(
        body["messages"],
        json!([{ "role": "user", "content": "hi" }])
    );
    assert!(body.get("tools").is_none());

    idle(b.activity()).await;
    let mut calls = Vec::new();
    for (_, path, _) in budget::daily_logs(dir.path()).unwrap() {
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let e: LogEvent = serde_json::from_str(line).unwrap();
            if e.event == "model.call" {
                calls.push(e);
            }
        }
    }
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.trace_id, req.trace_id);
    assert_eq!(call.fields["outcome"], "ok");
    assert_eq!(call.fields["stop_reason"], "stop");
    assert_eq!(call.fields["prompt_n"], 12);
    let blobs = telemetry.blobs().unwrap();
    let output: nebula_telemetry::BlobRef = call.fields["output_blob"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let output: serde_json::Value = serde_json::from_slice(&blobs.get(&output).unwrap()).unwrap();
    assert_eq!(output["text"], "Hello");
    let prompt: nebula_telemetry::BlobRef = call.fields["prompt_blob"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let prompt: serde_json::Value = serde_json::from_slice(&blobs.get(&prompt).unwrap()).unwrap();
    assert_eq!(prompt["messages"][0]["content"], "hi");
}

#[tokio::test]
async fn tool_calls_are_assembled_and_schema_and_tools_are_sent() {
    let state = FakeState::new();
    state.set_chunks(vec![
        json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "id": "call_1", "type": "function",
              "function": { "name": "read_file", "arguments": "{\"pa" } }
        ] }, "finish_reason": null }] }),
        json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "function": { "arguments": "th\":\"a.rs\"}" } }
        ] }, "finish_reason": null }] }),
        finish("tool_calls"),
    ]);
    let b = backend(&state).await;
    let mut req = hello();
    req.reasoning = ReasoningEffort::Medium;
    req.tools = vec![json!({ "type": "function", "function": { "name": "read_file" } })];
    req.response_schema = Some(json!({ "type": "object" }));
    let out = b.chat(req).await.unwrap().collect().await.unwrap();
    assert_eq!(out.stop_reason, Some(StopReason::ToolCalls));
    assert_eq!(out.tool_calls.len(), 1);
    assert_eq!(out.tool_calls[0].id, "call_1");
    assert_eq!(out.tool_calls[0].name, "read_file");
    assert_eq!(out.tool_calls[0].arguments, r#"{"path":"a.rs"}"#);

    let body = &state.requests("/v1/chat/completions")[0];
    assert_eq!(body["reasoning_effort"], "medium");
    assert_eq!(body["temperature"], 1.0);
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["type"],
        "object"
    );
}

#[tokio::test]
async fn http_errors_and_bad_keys_are_reported() {
    let state = FakeState::new();
    let b = backend(&state).await;
    state.chat_status.store(500, Ordering::SeqCst);
    match b.chat(hello()).await {
        Err(ModelError::Http { status: 500, body }) => assert!(body.contains("boom")),
        other => panic!("{other:?}", other = other.map(|_| ())),
    }
    *state.api_key.lock().unwrap() = Some("other".into());
    assert!(matches!(
        b.health().await,
        Err(ModelError::Http { status: 401, .. })
    ));
    assert_eq!(b.activity().in_flight(), 0);
}

#[tokio::test]
async fn cancel_all_ends_the_stream_as_cancelled() {
    let state = FakeState::new();
    state.set_chunks((0..200).map(|i| content(&format!("t{i} "))).collect());
    state.chunk_delay_ms.store(10, Ordering::SeqCst);
    let b = backend(&state).await;
    let mut stream = b.chat(hello()).await.unwrap();
    let first = stream.next().await.unwrap().unwrap();
    assert!(matches!(first, StreamItem::Token { .. }));
    assert_eq!(b.activity().in_flight(), 1);
    b.activity().cancel_all();
    let out = stream.collect().await.unwrap();
    assert_eq!(out.stop_reason, Some(StopReason::Cancelled));
    idle(b.activity()).await;
    assert!(matches!(
        b.chat(hello()).await,
        Err(ModelError::Unavailable(_))
    ));
}

#[tokio::test]
async fn dropping_the_stream_releases_the_request() {
    let state = FakeState::new();
    state.set_chunks((0..200).map(|i| content(&format!("t{i} "))).collect());
    state.chunk_delay_ms.store(10, Ordering::SeqCst);
    let b = backend(&state).await;
    let mut stream = b.chat(hello()).await.unwrap();
    stream.next().await.unwrap().unwrap();
    drop(stream);
    idle(b.activity()).await;
}

#[tokio::test]
async fn stream_without_finish_reason_is_an_error() {
    let state = FakeState::new();
    *state.chunks.lock().unwrap() = vec![content("partial").to_string()];
    let b = backend(&state).await;
    let err = b.chat(hello()).await.unwrap().collect().await.unwrap_err();
    assert!(matches!(err, ModelError::Protocol(_)), "{err}");
}

#[tokio::test]
async fn embed_tokenize_and_health() {
    let state = FakeState::new();
    let b = backend(&state).await;
    let v = b.embed(vec!["a".into(), "b".into()]).await.unwrap();
    assert_eq!(v, vec![vec![0.25, 0.75], vec![0.5, 0.5]]);
    assert_eq!(
        state.requests("/v1/embeddings")[0]["input"],
        json!(["a", "b"])
    );
    assert_eq!(b.tokenize("hello").await.unwrap(), vec![1, 2, 3]);
    assert_eq!(b.health().await.unwrap(), BackendHealth::Ok);
    state.health.store(503, Ordering::SeqCst);
    assert_eq!(b.health().await.unwrap(), BackendHealth::Loading);
}

// --- supervisor -------------------------------------------------------------------------

fn policy() -> SupervisorConfig {
    SupervisorConfig {
        health_interval: Duration::from_millis(20),
        startup_timeout: Duration::from_millis(500),
        hang_threshold: 3,
        backoff_base: Duration::from_millis(10),
        backoff_max: Duration::from_millis(80),
        failure_window: Duration::from_secs(60),
        max_failures: 3,
        drain_timeout: Duration::from_millis(200),
    }
}

fn manager(launcher: &Arc<FakeLauncher>) -> ModelManager {
    ModelManager::spawn(fake::config(), policy(), Arc::clone(launcher) as _, None).unwrap()
}

async fn wait_state(m: &ModelManager, s: ModelState) {
    let mut w = m.watch();
    tokio::time::timeout(Duration::from_secs(5), w.wait_for(|st| st.state == s))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {s:?}; status {:?}", m.status()))
        .unwrap();
}

#[tokio::test]
async fn supervisor_launches_and_serves() {
    let state = FakeState::new();
    state.set_chunks(vec![content("ok"), finish("stop")]);
    let launcher = FakeLauncher::new(Arc::clone(&state));
    let m = manager(&launcher);
    assert_eq!(m.status().state, ModelState::Stopped);
    assert!(m.backend().is_err());

    let status = m.set_profile("a").await.unwrap();
    assert_eq!(status.state, ModelState::Ready);
    assert_eq!(status.profile, "a");
    assert_eq!(m.pid(), Some(4242));

    let spec = &launcher.launches()[0];
    assert_eq!(
        spec.program,
        std::path::PathBuf::from("fake-llama-server.exe")
    );
    let args = spec.args.join(" ");
    assert!(args.starts_with("-m a.gguf -c 4096 -ctk f16 -ctv f16 --host 127.0.0.1 --port "));
    assert!(args.ends_with(" -ngl 99"));
    let key = &spec
        .env
        .iter()
        .find(|(k, _)| k == "LLAMA_API_KEY")
        .unwrap()
        .1;
    assert_eq!(key.len(), 64);

    // The fake checks the bearer token, so this also proves the key is wired through.
    let out = m
        .backend()
        .unwrap()
        .chat(hello())
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    assert_eq!(out.text, "ok");
}

#[tokio::test]
async fn crash_restarts_with_backoff() {
    let launcher = FakeLauncher::new(FakeState::new());
    let m = manager(&launcher);
    let mut events = m.subscribe();
    m.set_profile("a").await.unwrap();
    launcher.crash();
    wait_state(&m, ModelState::Restarting).await;
    wait_state(&m, ModelState::Ready).await;
    let s = m.status();
    assert_eq!(s.restarts, 1);
    assert!(s.last_error.unwrap().contains("exited"));
    assert_eq!(launcher.launches().len(), 2);

    let mut seen = Vec::new();
    while let Ok(e) = events.try_recv() {
        seen.push((e.from, e.to));
    }
    assert!(seen.contains(&(ModelState::Ready, ModelState::Restarting)));
    assert!(seen.contains(&(ModelState::Restarting, ModelState::Starting)));
}

#[tokio::test]
async fn unhealthy_server_is_restarted() {
    let state = FakeState::new();
    let launcher = FakeLauncher::new(Arc::clone(&state));
    let m = manager(&launcher);
    m.set_profile("a").await.unwrap();
    state.health.store(500, Ordering::SeqCst);
    wait_state(&m, ModelState::Restarting).await;
    assert!(m.status().last_error.unwrap().contains("unresponsive"));
    state.health.store(200, Ordering::SeqCst);
    wait_state(&m, ModelState::Ready).await;
}

#[tokio::test]
async fn repeated_crashes_end_in_failed_until_reloaded() {
    let launcher = FakeLauncher::new(FakeState::new());
    launcher.crash_on_start.store(true, Ordering::SeqCst);
    let m = manager(&launcher);
    let err = m.set_profile("a").await.unwrap_err();
    assert!(matches!(err, ModelError::Failed(_)), "{err}");
    let s = m.status();
    assert_eq!(s.state, ModelState::Failed);
    assert_eq!(s.restarts, 3);
    assert_eq!(launcher.launches().len(), 3);

    launcher.crash_on_start.store(false, Ordering::SeqCst);
    let s = m.set_profile("a").await.unwrap();
    assert_eq!(s.state, ModelState::Ready);
    assert_eq!(s.restarts, 0);
}

#[tokio::test]
async fn profile_switch_drains_then_cancels_in_flight_chat() {
    let state = FakeState::new();
    state.set_chunks((0..500).map(|i| content(&format!("t{i} "))).collect());
    state.chunk_delay_ms.store(10, Ordering::SeqCst);
    let launcher = FakeLauncher::new(Arc::clone(&state));
    let m = manager(&launcher);
    m.set_profile("a").await.unwrap();

    let mut stream = m.backend().unwrap().chat(hello()).await.unwrap();
    stream.next().await.unwrap().unwrap();
    wait_state(&m, ModelState::Busy).await;

    let (switched, out) = tokio::join!(m.set_profile("b"), stream.collect());
    assert_eq!(out.unwrap().stop_reason, Some(StopReason::Cancelled));
    let s = switched.unwrap();
    assert_eq!((s.profile.as_str(), s.state), ("b", ModelState::Ready));
    let launches = launcher.launches();
    assert_eq!(launches.len(), 2);
    assert!(launches[1].args.contains(&"b.gguf".to_owned()));
}

#[tokio::test]
async fn unknown_profile_stop_and_unload() {
    let launcher = FakeLauncher::new(FakeState::new());
    let m = manager(&launcher);
    assert!(matches!(
        m.set_profile("nope").await,
        Err(ModelError::UnknownProfile(_))
    ));
    m.set_profile("a").await.unwrap();
    // Same profile while ready is a no-op.
    m.set_profile("a").await.unwrap();
    assert_eq!(launcher.launches().len(), 1);

    let s = m.stop().await.unwrap();
    assert_eq!(s.state, ModelState::Stopped);
    assert!(launcher.current().exited());
    assert!(m.backend().is_err());
    assert_eq!(m.pid(), None);

    m.set_profile("a").await.unwrap();
    assert_eq!(m.unload().await.unwrap().state, ModelState::Unloaded);
    assert!(launcher.current().exited());
}

#[tokio::test]
async fn dropping_the_manager_stops_the_server() {
    let launcher = FakeLauncher::new(FakeState::new());
    let m = manager(&launcher);
    m.set_profile("a").await.unwrap();
    let ctl = launcher.current();
    drop(m);
    for _ in 0..100 {
        if ctl.exited() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("server still running after the manager was dropped");
}
