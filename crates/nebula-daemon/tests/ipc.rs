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
    ModelSetProfileParams, ModelState, ModelStatus, ResourceSnapshot, Role, ToolCallOutcome,
    ToolList, ToolsCallParams, TraceId, error_code,
};
use nebula_resources::Sources;
use nebula_resources::sources::{GpuReading, GpuSource, SystemReading, SystemSource};
use nebula_telemetry::{Telemetry, TelemetryConfig};
use nebula_tools::ToolHost;
use nebula_tools::testing::{FakeTool, host_config};

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
    /// Kept alive for the daemon's lifetime: the built-in file tools are confined to this
    /// directory (`tools.builtin.worktree_root`). `None` when the harness used the default root.
    _worktree: Option<tempfile::TempDir>,
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

async fn tool_host() -> ToolHost {
    let launcher = nebula_tools::testing::launcher(vec![FakeTool::echo()]);
    ToolHost::start_with(&host_config("fs", 1_000), None, launcher.as_ref())
        .await
        .unwrap()
}

fn deps(launcher: &Arc<FakeLauncher>, instance: &str, tool_host: ToolHost) -> Deps {
    Deps {
        telemetry: telemetry(),
        launcher: Arc::clone(launcher) as _,
        tool_host,
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

async fn start() -> Harness {
    start_configured(|_| {}).await
}

/// Starts a daemon whose built-in file tools are confined to a fresh temp directory.
///
/// `tools.builtin.worktree_root` is pointed at the tempdir and `resources.retired_drive` is set
/// to a drive letter the tempdir is *not* on (so the resolver does not reject the temp root as the
/// retired drive). Returns the harness (holding the tempdir alive) and the worktree root path.
async fn start_with_worktree() -> (Harness, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let retired = retired_drive_off(&root);
    let h = start_configured(move |cfg| {
        cfg.tools.builtin.worktree_root = root.clone();
        cfg.resources.retired_drive = retired.clone();
        cfg._worktree_dir = Some(dir);
    })
    .await;
    let root = h.config.tools.builtin.worktree_root.clone();
    (h, root)
}

/// A retired-drive letter guaranteed to differ from the drive `path` lives on, so a temp worktree
/// is never mistaken for the retired drive. Picks `Q:` unless `path` is on `Q:`, else `Z:`.
fn retired_drive_off(path: &std::path::Path) -> String {
    let on = path
        .components()
        .next()
        .and_then(|c| match c {
            std::path::Component::Prefix(p) => p.as_os_str().to_str(),
            _ => None,
        })
        .map(str::to_ascii_uppercase)
        .unwrap_or_default();
    if on.starts_with('Q') {
        "Z:".to_owned()
    } else {
        "Q:".to_owned()
    }
}

/// Shared start path: build the test config, let `tweak` adjust it (worktree root, retired drive,
/// and a tempdir to keep alive), then start the daemon.
async fn start_configured(tweak: impl FnOnce(&mut ConfigPatch)) -> Harness {
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
    let mut patch = ConfigPatch {
        config: config(&pipe_name),
        _worktree_dir: None,
    };
    tweak(&mut patch);
    let ConfigPatch {
        config,
        _worktree_dir,
    } = patch;
    let daemon = nebula_daemon::start(
        config.clone(),
        deps(&launcher, &instance, tool_host().await),
    )
    .unwrap();
    Harness {
        daemon,
        launcher,
        state,
        pipe: config.daemon.pipe_path(),
        config,
        instance,
        _worktree: _worktree_dir,
    }
}

/// A mutable config plus the tempdir its worktree root points into, so `start_configured`'s
/// closure can set both together and hand the tempdir to the [`Harness`] to keep alive.
struct ConfigPatch {
    config: NebulaConfig,
    _worktree_dir: Option<tempfile::TempDir>,
}

impl std::ops::Deref for ConfigPatch {
    type Target = NebulaConfig;
    fn deref(&self) -> &NebulaConfig {
        &self.config
    }
}

impl std::ops::DerefMut for ConfigPatch {
    fn deref_mut(&mut self) -> &mut NebulaConfig {
        &mut self.config
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
    let h = start().await;
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
    let h = start().await;
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

/// AC1 + AC4 over the pipe: list tools, call one, and see the `tool.call` event (with the trace
/// id) arrive on a `logs.subscribe` connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tools_list_call_and_log_reach_clients() {
    let h = start().await;
    let mut watcher = h.ready_client().await;
    let _: Empty = watcher
        .call(Method::LogsSubscribe(LogsSubscribeParams {
            target_prefix: Some("nebula_tools".into()),
            ..LogsSubscribeParams::default()
        }))
        .await
        .unwrap();

    let mut c = h.client().await;
    let list: ToolList = c.call(Method::ToolsList(Empty {})).await.unwrap();
    // The daemon now registers the ten in-process built-ins alongside the external `echo` tool
    // (task 14.1). `tools.list` returns them sorted by name.
    let echo = list
        .tools
        .iter()
        .find(|t| t.name == "echo")
        .expect("the external echo tool is listed");
    assert_eq!(echo.server, "fs");
    for name in [
        "fs.read",
        "fs.write",
        "fs.list",
        "fs.search",
        "git.status",
        "git.diff",
        "git.commit",
        "git.branch",
        "shell.run",
        "system.resources",
    ] {
        let builtin = list
            .tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("built-in {name} is listed"));
        assert_eq!(builtin.server, "builtin", "built-in {name} server label");
    }
    assert_eq!(list.tools.len(), 11, "ten built-ins plus the external echo");

    let trace = TraceId::new();
    let out: ToolCallOutcome = c
        .call(Method::ToolsCall(ToolsCallParams {
            tool: "echo".into(),
            arguments: serde_json::json!({ "message": "hi" }),
            trace_id: Some(trace),
        }))
        .await
        .unwrap();
    assert!(!out.is_error);

    tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Event::LogEvent(e) = watcher.next_event().await.unwrap()
                && e.event == "tool.call"
            {
                assert_eq!(e.fields.get("tool").and_then(|v| v.as_str()), Some("echo"));
                assert_eq!(e.fields.get("outcome").and_then(|v| v.as_str()), Some("ok"));
                assert_eq!(
                    e.trace_id,
                    Some(trace),
                    "the tool.call carries the trace id"
                );
                break;
            }
        }
    })
    .await
    .unwrap();
    h.daemon.shutdown().await;
}

// Task 14.2 coverage note — Job Object kill-on-close (Requirement 4.6):
//
// Proving a Windows Job Object terminates a child process *tree* requires `shell.run` to spawn a
// real, long-lived process and then observing the whole tree die when the job handle closes. That
// is a heavyweight, timing-sensitive OS test that does not belong behind the in-process pipe
// harness (whose launcher is the FakeLauncher and never spawns a real child). It is covered at the
// unit level in `nebula-tools` by the shell timeout/job test (task 9.5,
// `crates/nebula-tools/src/builtins/shell.rs`), which starts a sleeping command, lets the per-call
// timeout elapse, and asserts the child is killed via the job and `Timeout` is returned. The
// daemon-level tests here instead exercise the wiring that 14.2 can meaningfully assert over the
// pipe: built-in `tools.list`/`tools.call` round-trips, the `resources.snapshot` agreement, and the
// `tool.call` telemetry fields.

/// AC 1.4 / Req 2.2, 2.5 through the daemon: a built-in round-trips through the real `ToolHost`
/// and the `ToolCallOutcome` wire type, and the confinement provider the daemon installs is the
/// per-task [`TaskWorktreeProvider`](nebula_tools::TaskWorktreeProvider), not the old static
/// `ConfigWorktreeRoot` (issue #29, task 8.1).
///
/// Each `tools.call` runs on a fresh daemon-side task with no `CURRENT_WORKTREE` scope set — the
/// executor (issue #32) that would scope a task's root around its tool calls is not built yet. So
/// the per-task provider resolves the no-task-in-scope sentinel and every file built-in fails
/// closed at the confinement boundary, exactly as the sentinel seam requires (Req 2.5). The old
/// static-root behavior (fs.write/fs.read succeeding against `tools.builtin.worktree_root` with no
/// task in scope) is gone by design: with the per-task provider installed, nothing resolves
/// through a static root (Req 2.2). The round-trip over the wire type and MCP wrapping is still
/// exercised; only the resolution now fails closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tools_call_round_trips_a_builtin_through_the_daemon() {
    let (h, _root) = start_with_worktree().await;
    let mut c = h.client().await;

    // fs.write with no task in scope: the per-task provider yields the sentinel, which the
    // Path_Resolver rejects, so the built-in fails closed (Req 2.5) rather than writing against a
    // static root. The call round-trips through the host and surfaces an InvalidArguments error.
    let err = c
        .call::<ToolCallOutcome>(Method::ToolsCall(ToolsCallParams {
            tool: "fs.write".into(),
            arguments: serde_json::json!({ "path": "note.txt", "content": "hello builtin" }),
            trace_id: Some(TraceId::new()),
        }))
        .await
        .unwrap_err();
    assert_eq!(
        err.rpc_code(),
        Some(error_code::INVALID_PARAMS),
        "fs.write with no task in scope must fail closed at the confinement boundary: {err:?}",
    );

    // fs.read likewise fails closed with no task in scope: no built-in resolves through a static
    // root once the per-task provider is installed.
    let err = c
        .call::<ToolCallOutcome>(Method::ToolsCall(ToolsCallParams {
            tool: "fs.read".into(),
            arguments: serde_json::json!({ "path": "note.txt" }),
            trace_id: Some(TraceId::new()),
        }))
        .await
        .unwrap_err();
    assert_eq!(
        err.rpc_code(),
        Some(error_code::INVALID_PARAMS),
        "fs.read with no task in scope must fail closed at the confinement boundary: {err:?}",
    );

    h.daemon.shutdown().await;
}

/// Req 2.2 / 2.5 through the daemon, the `shell.run` seam: a `shell.run` built-in called with no
/// `CURRENT_WORKTREE` scope does **not** execute against any root — it fails closed at the
/// permission/confinement boundary. This complements
/// [`tools_call_round_trips_a_builtin_through_the_daemon`], which covers the `fs.*` resolver seam;
/// here we exercise the distinct way `shell.run` depends on the per-task provider.
///
/// # How `shell.run` reaches (or never reaches) the sentinel cwd
///
/// `shell.run` does not run its working directory through `path::resolve` the way `fs.*` do.
/// Instead it classifies the command first (before any child is spawned), and only on an
/// execute decision does it read `ctx.worktree.worktree_root()` and pin it as the child's
/// `current_dir` (see `crates/nebula-tools/src/builtins/shell.rs`). The daemon installs the
/// per-task [`TaskWorktreeProvider`](nebula_tools::TaskWorktreeProvider), so with no task in scope
/// that root is the no-task-in-scope sentinel (`C:\nebula\no-task-in-scope`) — a path on the
/// retired drive that is never created and never touched.
///
/// There are therefore two fail-closed shapes, and `shell.run` always lands on one of them with no
/// task in scope — it never succeeds against a static root:
///
/// 1. **Refused at the classification gate (asserted here).** The daemon's real `RulesClassifier`
///    classifies an unknown command as `Tier::System` and injects no approval, so the gate refuses
///    it with [`ToolError::InvalidArguments`] (→ `INVALID_PARAMS`) *before* any child is spawned
///    and *before* the sentinel cwd is ever read. This is deterministic and, crucially, never asks
///    the OS to resolve the `C:` sentinel directory, honoring AGENTS.md hard rule 5 (never touch
///    `C:`).
/// 2. **Spawn-time failure (not exercised, by design).** A command that classified at or below the
///    no-approval threshold would reach `execute`, read the sentinel as its `current_dir`, and
///    fail to spawn (→ [`ToolError::Unavailable`] / `MODEL_UNAVAILABLE`) because that directory
///    does not exist. We deliberately do *not* drive this path: pinning `current_dir` to a `C:`
///    path would ask the OS to resolve a path on the retired drive, which hard rule 5 forbids. The
///    gate-refusal path proves "fails closed, never runs against a static root" without touching
///    `C:` at all.
///
/// # Req 2.2 proof (explicit)
///
/// The provider the daemon installs is the per-task
/// [`TaskWorktreeProvider`](nebula_tools::TaskWorktreeProvider), **not** the old static
/// `ConfigWorktreeRoot` (issue #29, task 8.1). The observable proof is behavioral: with no task in
/// scope the built-in fails closed. Under the old static-root wiring, `shell.run` would have read
/// `tools.builtin.worktree_root` (the real tempdir below) as a perfectly valid, existing working
/// directory and the command would have had a chance to *run* against it. It cannot here — nothing
/// resolves through a static root once the per-task provider is installed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_task_in_scope_fails_closed_through_the_daemon() {
    // `tools.builtin.worktree_root` is a real, existing tempdir. Under the OLD static-root wiring
    // this is exactly the directory a built-in would have resolved/run against with no task in
    // scope. The per-task provider ignores it when no `CURRENT_WORKTREE` scope is active, so its
    // existence is what makes the fail-closed assertions below meaningful (they are NOT failing
    // merely because the configured root is missing).
    let (h, root) = start_with_worktree().await;
    assert!(
        root.is_dir(),
        "the configured static worktree root exists ({root:?}); the old wiring would have run \
         built-ins against it with no task in scope",
    );
    let mut c = h.client().await;

    // `shell.run` with no `CURRENT_WORKTREE` scope. "nebula-no-such-binary" matches no rule in the
    // embedded rules table, so the daemon's `RulesClassifier` classifies it as `Tier::System` with
    // no approval. The gate refuses it (`INVALID_PARAMS`) BEFORE any child is spawned and before
    // the sentinel cwd is read — the command never executes against any root, static or otherwise.
    let err = c
        .call::<ToolCallOutcome>(Method::ToolsCall(ToolsCallParams {
            tool: "shell.run".into(),
            arguments: serde_json::json!({ "command": "nebula-no-such-binary" }),
            trace_id: Some(TraceId::new()),
        }))
        .await
        .unwrap_err();
    // Fail closed: the call is an ERROR, never a successful outcome. The code is a confinement/
    // permission failure. We accept either fail-closed shape documented above so the test is robust
    // to classifier-table changes, but assert it is specifically NOT a success and NOT an
    // unrelated error (not-found / internal):
    //   - INVALID_PARAMS: refused at the classification gate (the path this command takes); or
    //   - MODEL_UNAVAILABLE: spawn refused because the sentinel cwd does not exist.
    let code = err.rpc_code();
    assert!(
        matches!(
            code,
            Some(error_code::INVALID_PARAMS | error_code::MODEL_UNAVAILABLE)
        ),
        "shell.run with no task in scope must fail closed at the confinement boundary, never run \
         against a static root: {err:?}",
    );

    h.daemon.shutdown().await;
}

/// AC 6.4 through the daemon: `system.resources` (via `tools.call`) returns exactly what
/// `resources.snapshot` returns for the same sampler state. The harness runs with the sampler
/// enabled (fake GPU/system sources), so once a snapshot exists both paths agree field-for-field.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn system_resources_matches_resources_snapshot() {
    let h = start().await;
    let mut c = h.ready_client().await;

    // Wait for the sampler to publish its first snapshot (same poll the protocol test uses).
    let snapshot: ResourceSnapshot = tokio::time::timeout(TIMEOUT, async {
        loop {
            match c.call(Method::ResourcesSnapshot(Empty {})).await {
                Ok(s) => return s,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .unwrap();

    // `system.resources` reads the identical watch channel, so its JSON output deserializes back
    // to a snapshot equal field-for-field. (The sampler publishes a stable snapshot between ticks,
    // and the fake sources return fixed readings, so the two reads observe the same value.)
    let out: ToolCallOutcome = c
        .call(Method::ToolsCall(ToolsCallParams {
            tool: "system.resources".into(),
            arguments: serde_json::json!({}),
            trace_id: None,
        }))
        .await
        .unwrap();
    assert!(!out.is_error, "system.resources should succeed: {out:?}");
    let text = out.content[0]["text"].as_str().expect("text content block");
    let via_tool: ResourceSnapshot =
        serde_json::from_str(text).expect("system.resources output is a ResourceSnapshot");

    // Fixed fake readings mean the stable fields match the snapshot read moments earlier.
    assert_eq!(via_tool.vram_total_mib, snapshot.vram_total_mib);
    assert_eq!(via_tool.vram_total_mib, 12_282);
    assert_eq!(via_tool.ram_total_mib, snapshot.ram_total_mib);
    assert_eq!(via_tool.gpu_processes.len(), snapshot.gpu_processes.len());
    assert_eq!(
        via_tool.gpu_processes.first().map(|p| p.name.as_str()),
        Some("llama-server.exe"),
    );

    h.daemon.shutdown().await;
}

/// AC 8.1/8.3/8.5 through the daemon: a built-in call emits a `tool.call` telemetry event carrying
/// the outcome, the elapsed `wall_ms`, and the trace id. Subscribes on the `nebula_tools` prefix,
/// calls `system.resources`, and asserts the event's fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn builtin_call_emits_telemetry_with_outcome_and_wall_ms() {
    let h = start().await;
    let mut watcher = h.ready_client().await;
    let _: Empty = watcher
        .call(Method::LogsSubscribe(LogsSubscribeParams {
            target_prefix: Some("nebula_tools".into()),
            ..LogsSubscribeParams::default()
        }))
        .await
        .unwrap();

    let mut c = h.client().await;
    let trace = TraceId::new();
    let out: ToolCallOutcome = c
        .call(Method::ToolsCall(ToolsCallParams {
            tool: "system.resources".into(),
            arguments: serde_json::json!({}),
            trace_id: Some(trace),
        }))
        .await
        .unwrap();
    assert!(!out.is_error);

    tokio::time::timeout(TIMEOUT, async {
        loop {
            if let Event::LogEvent(e) = watcher.next_event().await.unwrap()
                && e.event == "tool.call"
                && e.fields.get("tool").and_then(|v| v.as_str()) == Some("system.resources")
            {
                // Outcome is the success/failure classification (Req 8.3). The sampler may or may
                // not have produced a snapshot yet, so accept either "ok" or the "error"
                // (Unavailable) outcome — both are valid, and both must still be logged (Req 8.4).
                let outcome = e.fields.get("outcome").and_then(|v| v.as_str());
                assert!(
                    matches!(outcome, Some("ok" | "error")),
                    "outcome field present and classified: {outcome:?}"
                );
                // Elapsed duration is recorded as `wall_ms` (Req 8.3).
                assert!(
                    e.fields
                        .get("wall_ms")
                        .and_then(serde_json::Value::as_u64)
                        .is_some(),
                    "wall_ms field present: {:?}",
                    e.fields.get("wall_ms")
                );
                // The trace id inherited from the boundary (Req 8.2) is carried.
                assert_eq!(
                    e.trace_id,
                    Some(trace),
                    "the tool.call carries the trace id"
                );
                break;
            }
        }
    })
    .await
    .unwrap();
    h.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_chat_concurrently() {
    let h = start().await;
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
    let h = start().await;
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
    let h = start().await;
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
    let h = start().await;
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

    c.send_line(r#"{"jsonrpc":"2.0","id":"x","proto_version":2,"method":"nope","params":{}}"#)
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
    let h = start().await;
    let launcher = FakeLauncher::new(FakeState::new());
    let err = nebula_daemon::start(
        h.config.clone(),
        deps(&launcher, &h.instance, tool_host().await),
    )
    .err()
    .unwrap();
    assert!(matches!(err, DaemonError::AlreadyRunning), "{err}");
    assert!(launcher.launches().is_empty());

    // A different mutex but the same pipe name: the pipe is already owned.
    let other = format!(r"Local\NebulaDaemonTest-{}", ChatId::new());
    let err = nebula_daemon::start(h.config.clone(), deps(&launcher, &other, tool_host().await))
        .err()
        .unwrap();
    assert!(matches!(err, DaemonError::Pipe { .. }), "{err}");
    h.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_request_stops_everything() {
    let h = start().await;
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
    let again = nebula_daemon::start(
        h.config.clone(),
        deps(&launcher, &h.instance, tool_host().await),
    )
    .unwrap();
    again.shutdown().await;
}
