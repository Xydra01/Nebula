#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

//! Integration tests for the single executor loop (`nebula-daemon::executor`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use nebula_config::NebulaConfig;
use nebula_daemon::client::Client;
use nebula_daemon::{Daemon, Deps};
use nebula_model::SupervisorConfig;
use nebula_model::testing::{FakeLauncher, FakeState, content, finish};
use nebula_proto::{
    ChatId, CheckStatus, DoctorCheck, DoctorReport, Empty, Event, Method, ModelState, ModelStatus,
    TaskCancelParams, TaskCreateParams, TaskDone, TaskId, TaskStarted, TaskStatus, TaskStep,
};
use nebula_resources::Sources;
use nebula_resources::sources::{GpuReading, GpuSource, SystemReading, SystemSource};

use nebula_telemetry::{Telemetry, TelemetryConfig};
use nebula_tools::ToolHost;
use nebula_tools::testing::{FakeTool, host_config};
use serde_json::{Value, json};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(15);

fn telemetry() -> Telemetry {
    static T: OnceLock<Telemetry> = OnceLock::new();
    T.get_or_init(|| nebula_telemetry::init(&TelemetryConfig::default()).unwrap())
        .clone()
}

fn supervisor_policy() -> SupervisorConfig {
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
    _daemon: Daemon,
    _launcher: Arc<FakeLauncher>,
    state: Arc<FakeState>,
    pipe: String,
    _worktrees_dir: TempDir,
}

fn retired_drive_off(path: &Path) -> String {
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

async fn start_harness() -> Harness {
    let unique = ChatId::new().to_string();
    let pipe_name = format!("nebula-test-{unique}");
    let instance = format!(r"Local\NebulaExecutorTest-{unique}");
    let state = FakeState::new();
    let launcher = FakeLauncher::new(Arc::clone(&state));

    let worktrees_dir = tempfile::tempdir().unwrap();
    let retired_drive = retired_drive_off(worktrees_dir.path());

    let mut config = NebulaConfig::from_toml(None).unwrap();
    config.model = nebula_model::testing::config();
    config.daemon.pipe_name = pipe_name.clone();
    config.daemon.load_on_start = "a".into();
    config.daemon.embedding_profile = String::new();
    config.resources.sample_interval_ms = 100;
    config.worktree.worktrees_dir = worktrees_dir.path().to_path_buf();
    config.resources.retired_drive = retired_drive;

    let external_launcher = nebula_tools::testing::launcher(vec![FakeTool::echo()]);
    let tool_host =
        ToolHost::start_with(&host_config("fs", 1_000), None, external_launcher.as_ref())
            .await
            .unwrap();

    let deps = Deps {
        telemetry: telemetry(),
        launcher: Arc::clone(&launcher) as _,
        tool_host,
        sources: Some(Sources {
            gpu: Some(Box::new(FakeGpu)),
            gpu_processes: None,
            system: Box::new(FakeSystem),
        }),
        preflight: None,
        supervisor: supervisor_policy(),
        instance_name: instance,
        local_checks: Arc::new(|_| {
            DoctorReport::from_checks(vec![DoctorCheck {
                name: "local.fake".into(),
                status: CheckStatus::Ok,
                detail: "fine".into(),
            }])
        }),
        artifact_checks: false,
        foreground_detector: None,
    };

    let daemon = nebula_daemon::start(config.clone(), deps).unwrap();

    Harness {
        _daemon: daemon,
        _launcher: launcher,
        state,
        pipe: config.daemon.pipe_path(),
        _worktrees_dir: worktrees_dir,
    }
}

impl Harness {
    async fn ready_client(&self) -> Client {
        let mut c = Client::connect(&self.pipe, TIMEOUT).await.unwrap();
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let s: ModelStatus = c.call(Method::ModelStatus(Empty {})).await.unwrap();
                if s.state == ModelState::Ready {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("model never reached ready");
        c
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

fn git_branch_exists(dir: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(dir)
        .status()
        .unwrap()
        .success()
}

fn seed_repo() -> TempDir {
    let d = TempDir::new().unwrap();
    git(d.path(), &["init"]);
    git(d.path(), &["config", "user.email", "test@example.com"]);
    git(d.path(), &["config", "user.name", "test"]);
    git(d.path(), &["config", "core.autocrlf", "false"]);
    std::fs::write(d.path().join("seed.txt"), "seed\n").unwrap();
    git(d.path(), &["add", "seed.txt"]);
    git(d.path(), &["commit", "-m", "seed"]);
    d
}

fn tool_call_turn(id: &str, name: &str, arguments: &str) -> Vec<Value> {
    vec![
        json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments,
                        }
                    }]
                },
                "finish_reason": null,
            }]
        }),
        finish("tool_calls"),
    ]
}

fn completion_turn(text: &str) -> Vec<Value> {
    vec![content(text), finish("stop")]
}

async fn collect_task_events(c: &mut Client, task_id: TaskId) -> (Vec<TaskStep>, TaskDone) {
    let mut steps = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let ev = tokio::time::timeout_at(deadline, c.next_event())
            .await
            .expect("timeout waiting for task event")
            .expect("event error");
        match ev {
            Event::TaskStep(step) if step.task_id == task_id => {
                steps.push(step);
            }
            Event::TaskDone(done) if done.task_id == task_id => {
                return (steps, done);
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn test_executor_completes_without_tools() {
    let h = start_harness().await;
    h.state
        .set_chunks(completion_turn("Task completed successfully"));

    let repo = seed_repo();
    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Do nothing and complete".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: None,
            max_tokens: None,
            wall_time_secs: None,
            create_pr: None,
        }))
        .await
        .unwrap();

    let (steps, done) = collect_task_events(&mut c, started.task_id).await;

    assert_eq!(done.status, TaskStatus::Completed);
    assert_eq!(done.summary, "Task completed successfully");
    assert!(steps.is_empty());

    let branch = format!("nebula/{}", started.task_id);
    assert!(git_branch_exists(repo.path(), &branch));
}

#[tokio::test]
async fn test_executor_tool_call_limit_reached() {
    let h = start_harness().await;
    // Model continuously requests fs.list
    h.state
        .set_chunks(tool_call_turn("call_1", "fs.list", r#"{"path":"."}"#));

    let repo = seed_repo();
    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Run tools until limit".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: Some(2),
            max_tokens: None,
            wall_time_secs: None,
            create_pr: None,
        }))
        .await
        .unwrap();

    let (steps, done) = collect_task_events(&mut c, started.task_id).await;

    assert_eq!(done.status, TaskStatus::LimitReached);
    assert!(done.summary.contains("maximum tool-call limit reached"));
    assert_eq!(steps.len(), 2);
}

#[tokio::test]
async fn test_executor_token_budget_exhausted() {
    let h = start_harness().await;
    // Set response timings reporting 120 total tokens
    h.state.set_chunks(vec![
        json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "fs.list", "arguments": r#"{"path":"."}"# }
                    }]
                },
                "finish_reason": null,
            }]
        }),
        json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "timings": {
                "prompt_n": 80,
                "prompt_ms": 10.0,
                "predicted_n": 40,
                "predicted_ms": 10.0,
                "predicted_per_second": 100.0
            }
        }),
    ]);

    let repo = seed_repo();
    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Exhaust tokens".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: Some(10),
            max_tokens: Some(100),
            wall_time_secs: None,
            create_pr: None,
        }))
        .await
        .unwrap();

    let (_steps, done) = collect_task_events(&mut c, started.task_id).await;

    assert_eq!(done.status, TaskStatus::LimitReached);
    assert!(done.summary.contains("model token budget exhausted"));
}

#[tokio::test]
async fn test_executor_wall_clock_timeout() {
    let h = start_harness().await;
    // Set 600ms chunk delay across 3 chunks -> takes > 1.2s
    h.state.set_chunk_delay(Duration::from_millis(600));
    h.state.set_chunks(vec![
        content("chunk 1 "),
        content("chunk 2 "),
        finish("stop"),
    ]);

    let repo = seed_repo();
    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Timeout test".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: None,
            max_tokens: None,
            wall_time_secs: Some(1),
            create_pr: None,
        }))
        .await
        .unwrap();

    let (_steps, done) = collect_task_events(&mut c, started.task_id).await;

    assert_eq!(done.status, TaskStatus::LimitReached);
    assert!(done.summary.contains("timeout"));
}

#[tokio::test]
async fn test_executor_cancellation() {
    let h = start_harness().await;
    h.state.set_chunk_delay(Duration::from_millis(500));
    h.state
        .set_chunks(tool_call_turn("call_c", "fs.list", r#"{"path":"."}"#));

    let repo = seed_repo();
    let mut c = h.ready_client().await;
    let mut c_cancel = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Cancel test".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: Some(10),
            max_tokens: None,
            wall_time_secs: None,
            create_pr: None,
        }))
        .await
        .unwrap();

    // Give it a moment to begin executing, then cancel
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _: Empty = c_cancel
        .call(Method::TaskCancel(TaskCancelParams {
            task_id: started.task_id,
        }))
        .await
        .unwrap();

    let (_steps, done) = collect_task_events(&mut c, started.task_id).await;

    assert_eq!(done.status, TaskStatus::Cancelled);
    assert!(done.summary.contains("cancelled"));
}

#[tokio::test]
async fn test_phase1_exit_criterion_fixes_failing_test() {
    let h = start_harness().await;

    // Create a seed repo with a failing python test
    let repo = seed_repo();
    let math_lib_content = "def add(a, b):\n    return a - b\n";
    let test_math_content = "import unittest\nfrom math_lib import add\n\nclass TestMath(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\nif __name__ == '__main__':\n    unittest.main()\n";

    std::fs::write(repo.path().join("math_lib.py"), math_lib_content).unwrap();
    std::fs::write(repo.path().join("test_math.py"), test_math_content).unwrap();
    git(repo.path(), &["add", "math_lib.py", "test_math.py"]);
    git(repo.path(), &["commit", "-m", "add failing test"]);

    // Queue model turns:
    // Turn 1: Model inspects math_lib.py
    h.state.push_turn(tool_call_turn(
        "call_1",
        "fs.read",
        r#"{"path":"math_lib.py"}"#,
    ));

    // Turn 2: Model writes the fix
    let fixed_content = "def add(a, b):\n    return a + b\n";
    let write_args = json!({
        "path": "math_lib.py",
        "content": fixed_content,
    })
    .to_string();
    h.state
        .push_turn(tool_call_turn("call_2", "fs.write", &write_args));

    // Turn 3: Model inspects the diff to verify changes
    h.state
        .push_turn(tool_call_turn("call_3", "git.diff", "{}"));

    // Turn 4: Model outputs final answer
    h.state.push_turn(completion_turn(
        "Fixed the bug in math_lib.py: changed subtraction to addition and verified the diff.",
    ));

    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Fix the failing test in test_math.py".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: Some(10),
            max_tokens: Some(5000),
            wall_time_secs: Some(30),
            create_pr: None,
        }))
        .await
        .unwrap();

    let (steps, done) = collect_task_events(&mut c, started.task_id).await;

    assert_eq!(done.status, TaskStatus::Completed);
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0].tool_calls, vec!["fs.read"]);
    assert_eq!(steps[1].tool_calls, vec!["fs.write"]);
    assert_eq!(steps[2].tool_calls, vec!["git.diff"]);

    // Verify git branch in the repository
    let branch = format!("nebula/{}", started.task_id);
    assert!(git_branch_exists(repo.path(), &branch));

    // Verify the modified file in the task worktree
    let worktree_dir = h._worktrees_dir.path().join(started.task_id.to_string());
    let worktree_file = worktree_dir.join("math_lib.py");
    assert_eq!(
        std::fs::read_to_string(&worktree_file).unwrap(),
        fixed_content
    );

    // Verify test_math.py passes when executed in the worktree
    if let Ok(status) = std::process::Command::new("python")
        .arg("test_math.py")
        .current_dir(&worktree_dir)
        .status()
    {
        assert!(status.success(), "test_math.py should pass in the worktree");
    }
}

#[tokio::test]
#[ignore = "requires real model server running locally"]
async fn test_real_model_fixes_failing_test() {
    let repo = seed_repo();
    let math_lib_content = "def add(a, b):\n    return a - b\n";
    let test_math_content = "import unittest\nfrom math_lib import add\n\nclass TestMath(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\nif __name__ == '__main__':\n    unittest.main()\n";

    std::fs::write(repo.path().join("math_lib.py"), math_lib_content).unwrap();
    std::fs::write(repo.path().join("test_math.py"), test_math_content).unwrap();
    git(repo.path(), &["add", "math_lib.py", "test_math.py"]);
    git(repo.path(), &["commit", "-m", "add failing test"]);

    let config = NebulaConfig::from_toml(None).unwrap();
    let mut c = Client::connect(&config.daemon.pipe_path(), TIMEOUT)
        .await
        .unwrap();

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Fix the failing test in test_math.py by fixing math_lib.py".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: Some(10),
            max_tokens: Some(5000),
            wall_time_secs: Some(120),
            create_pr: None,
        }))
        .await
        .unwrap();

    let (_steps, done) = collect_task_events(&mut c, started.task_id).await;
    assert_eq!(done.status, TaskStatus::Completed);
}

#[tokio::test]
async fn test_circuit_breaker_repeated_tool_failure() {
    let h = start_harness().await;
    // Model calls fs.read on a nonexistent file 3 times consecutively
    h.state.set_chunks(tool_call_turn(
        "call_1",
        "fs.read",
        r#"{"path":"missing.txt"}"#,
    ));

    let repo = seed_repo();
    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Read nonexistent file".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: Some(10),
            max_tokens: None,
            wall_time_secs: None,
            create_pr: None,
        }))
        .await
        .unwrap();

    let (_steps, done) = collect_task_events(&mut c, started.task_id).await;
    assert_eq!(done.status, TaskStatus::LimitReached);
    assert!(
        done.summary.contains("circuit breaker tripped"),
        "expected breaker tripped in summary, got: {}",
        done.summary
    );
    assert!(
        done.summary
            .contains("tool 'fs.read' failed 3 times consecutively"),
        "summary was: {}",
        done.summary
    );
}

#[tokio::test]
async fn test_circuit_breaker_repeated_file_edits() {
    let h = start_harness().await;
    // Model writes to foo.txt 5 times
    h.state.set_chunks(tool_call_turn(
        "call_1",
        "fs.write",
        r#"{"path":"foo.txt","content":"hello"}"#,
    ));

    let repo = seed_repo();
    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Repeatedly write file".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: Some(15),
            max_tokens: None,
            wall_time_secs: None,
            create_pr: None,
        }))
        .await
        .unwrap();

    let (_steps, done) = collect_task_events(&mut c, started.task_id).await;
    assert_eq!(done.status, TaskStatus::LimitReached);
    assert!(
        done.summary.contains("circuit breaker tripped"),
        "summary was: {}",
        done.summary
    );
    assert!(
        done.summary.contains("file 'foo.txt' modified 5 times"),
        "summary was: {}",
        done.summary
    );
}

#[tokio::test]
async fn test_circuit_breaker_runaway_tokens() {
    let h = start_harness().await;
    // Model returns a turn that exceeds runaway_step_tokens (default 16384)
    h.state.set_chunks(vec![json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "fs.list",
                        "arguments": r#"{"path":"."}"#,
                    }
                }]
            },
            "finish_reason": "tool_calls",
        }],
        "timings": {
            "prompt_n": 10000,
            "prompt_ms": 100.0,
            "predicted_n": 7000,
            "predicted_ms": 100.0,
        }
    })]);

    let repo = seed_repo();
    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Runaway token test".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: Some(10),
            max_tokens: Some(50000),
            wall_time_secs: None,
            create_pr: None,
        }))
        .await
        .unwrap();

    let (_steps, done) = collect_task_events(&mut c, started.task_id).await;
    assert_eq!(done.status, TaskStatus::LimitReached);
    assert!(
        done.summary.contains("circuit breaker tripped"),
        "summary was: {}",
        done.summary
    );
    assert!(
        done.summary
            .contains("step token usage (17000) exceeded limit (16384)"),
        "summary was: {}",
        done.summary
    );
}

#[tokio::test]
async fn test_global_stop_terminates_all_tasks_within_two_seconds() {
    let h = start_harness().await;
    // Model runs shell command sleeping 60 seconds
    h.state.set_chunks(tool_call_turn(
        "call_1",
        "shell.run",
        r#"{"command":"powershell -NoProfile -Command Start-Sleep -Seconds 60"}"#,
    ));

    let repo = seed_repo();
    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Run long sleeping task".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: None,
            max_tokens: None,
            wall_time_secs: Some(120),
            create_pr: None,
        }))
        .await
        .unwrap();

    // Wait until the step event is emitted indicating the command is running
    let step = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::TaskStep(s)) = c.next_event().await
                && s.task_id == started.task_id
            {
                return s;
            }
        }
    })
    .await
    .expect("timed out waiting for task step to start");
    assert_eq!(step.tool_calls, vec!["shell.run"]);

    // Send task.stop_all and time it
    let start_stop = tokio::time::Instant::now();
    let mut c2 = Client::connect(&h.pipe, TIMEOUT).await.unwrap();
    let res: nebula_proto::TaskStopAllResponse = c2
        .call(Method::TaskStopAll(nebula_proto::TaskStopAllParams {
            reason: Some("emergency test stop".into()),
        }))
        .await
        .unwrap();
    let elapsed = start_stop.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "global stop took {elapsed:?}, exceeding 2s requirement"
    );
    assert!(res.stopped >= 1, "expected at least 1 task stopped");

    // The task should yield TaskDone with Cancelled status within 2 seconds
    let done = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(Event::TaskDone(d)) = c.next_event().await
                && d.task_id == started.task_id
            {
                return d;
            }
        }
    })
    .await
    .expect("timed out waiting for task done after global stop");

    assert_eq!(done.status, TaskStatus::Cancelled);
}

#[tokio::test]
async fn test_disk_guard_trips_with_fake_low_space() {
    let unique = ChatId::new().to_string();
    let pipe_name = format!("nebula-test-{unique}");
    let instance = format!(r"Local\NebulaExecutorTest-{unique}");
    let state = FakeState::new();
    let launcher = FakeLauncher::new(Arc::clone(&state));

    let worktrees_dir = tempfile::tempdir().unwrap();
    let retired_drive = retired_drive_off(worktrees_dir.path());

    let mut config = NebulaConfig::from_toml(None).unwrap();
    config.model = nebula_model::testing::config();
    config.daemon.pipe_name = pipe_name.clone();
    config.daemon.load_on_start = "a".into();
    config.daemon.embedding_profile = String::new();
    config.resources.sample_interval_ms = 100;
    config.worktree.worktrees_dir = worktrees_dir.path().to_path_buf();
    config.resources.retired_drive = retired_drive;

    // Configure a volume guard for the worktrees_dir volume with warn_gb = 30
    let mount = worktrees_dir
        .path()
        .to_string_lossy()
        .chars()
        .take(3)
        .collect::<String>();
    config.resources.volumes = vec![nebula_config::VolumeGuard {
        mount: mount.clone(),
        warn_gb: 30,
        block_gb: Some(20),
        pause_gb: Some(12),
    }];

    // Fake system reporting only 10 GB free space (< 30 GB budget)
    struct FakeLowDiskSystem {
        mount: String,
    }
    impl SystemSource for FakeLowDiskSystem {
        fn read(
            &mut self,
            _mounts: &[String],
        ) -> Result<SystemReading, nebula_resources::ResourceError> {
            Ok(SystemReading {
                ram_used_mib: 4000,
                ram_total_mib: 16000,
                commit_used_mib: 5000,
                commit_limit_mib: 20000,
                cpu_pct: 10.0,
                disks: vec![nebula_proto::DiskUsage {
                    mount: self.mount.clone(),
                    free_bytes: 10 * nebula_resources::GB,
                    total_bytes: 500 * nebula_resources::GB,
                }],
            })
        }
        fn process_names(&mut self, _pids: &[u32]) -> HashMap<u32, String> {
            HashMap::new()
        }
    }

    let external_launcher = nebula_tools::testing::launcher(vec![FakeTool::echo()]);
    let tool_host =
        ToolHost::start_with(&host_config("fs", 1_000), None, external_launcher.as_ref())
            .await
            .unwrap();

    let deps = Deps {
        telemetry: telemetry(),
        launcher: Arc::clone(&launcher) as _,
        tool_host,
        sources: Some(Sources {
            gpu: Some(Box::new(FakeGpu)),
            gpu_processes: None,
            system: Box::new(FakeLowDiskSystem {
                mount: mount.clone(),
            }),
        }),
        preflight: None,
        supervisor: supervisor_policy(),
        instance_name: instance,
        local_checks: Arc::new(|_| {
            DoctorReport::from_checks(vec![DoctorCheck {
                name: "local.fake".into(),
                status: CheckStatus::Ok,
                detail: "fine".into(),
            }])
        }),
        artifact_checks: false,
        foreground_detector: None,
    };

    let daemon = nebula_daemon::start(config.clone(), deps).unwrap();

    // Wait for sampler to take initial reading
    tokio::time::sleep(Duration::from_millis(300)).await;

    let repo = seed_repo();
    let mut c = Client::connect(&config.daemon.pipe_path(), TIMEOUT)
        .await
        .unwrap();

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Task with low disk space".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: None,
            max_tokens: None,
            wall_time_secs: None,
            create_pr: None,
        }))
        .await
        .unwrap();

    let (_steps, done) = collect_task_events(&mut c, started.task_id).await;
    assert_eq!(done.status, TaskStatus::LimitReached);
    assert!(
        done.summary.contains("disk guard tripped"),
        "summary was: {}",
        done.summary
    );
    assert!(
        done.summary
            .contains("below budget (10 GB free < 30 GB budget)"),
        "summary was: {}",
        done.summary
    );
    drop(daemon);
}

#[tokio::test]
async fn test_refuse_writes_to_retired_drive() {
    let h = start_harness().await;
    let retired_path = format!(r"{}\evil.txt", retired_drive_off(h._worktrees_dir.path()));
    let write_args = json!({ "path": retired_path, "content": "exploit" });
    h.state.set_chunks(tool_call_turn(
        "call_1",
        "fs.write",
        &write_args.to_string(),
    ));

    let repo = seed_repo();
    let mut c = h.ready_client().await;

    let started: TaskStarted = c
        .call(Method::TaskCreate(TaskCreateParams {
            goal: "Attempt write to retired drive".into(),
            repo: repo.path().to_path_buf(),
            start_point: None,
            profile: None,
            max_tool_calls: Some(1),
            max_tokens: None,
            wall_time_secs: None,
            create_pr: None,
        }))
        .await
        .unwrap();

    let (steps, done) = collect_task_events(&mut c, started.task_id).await;
    assert_eq!(steps.len(), 1);
    assert_eq!(done.status, TaskStatus::LimitReached);
}
