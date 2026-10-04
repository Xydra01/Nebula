#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::path::Path;
use std::process::Command as StdCommand;
use std::sync::Arc;
use std::time::Duration;

use nebula_model::{
    ChatRequest, LaunchSpec, Launcher, ModelBackend, ModelConfig, ModelManager, ProcessLauncher,
    SupervisorConfig,
};
use nebula_proto::{ChatMessage, ModelState, Role};

fn spec(program: &str, args: &[&str]) -> LaunchSpec {
    LaunchSpec {
        program: program.into(),
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        env: vec![("LLAMA_API_KEY".into(), "unused".into())],
        port: 0,
    }
}

fn alive(pid: u32) -> bool {
    let out = StdCommand::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

fn children_of(pid: u32) -> Vec<u32> {
    let script = format!(
        "Get-CimInstance Win32_Process -Filter 'ParentProcessId={pid}' | ForEach-Object {{ $_.ProcessId }}"
    );
    let out = StdCommand::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

#[tokio::test]
async fn stop_kills_and_output_is_captured() {
    let mut p = ProcessLauncher::default()
        .launch(spec("ping", &["-n", "30", "127.0.0.1"]))
        .await
        .unwrap();
    let pid = p.pid().unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(p.try_exit().is_none());
    assert!(p.recent_output().iter().any(|l| l.contains("127.0.0.1")));
    p.stop().await;
    assert!(p.try_exit().is_some());
    assert!(!alive(pid));
}

#[tokio::test]
async fn job_object_kills_grandchildren_when_dropped() {
    // cmd starts ping as its own child. Dropping the handle kills cmd (kill_on_drop) and
    // closing the Job Object must take ping with it.
    let p = ProcessLauncher::default()
        .launch(spec("cmd", &["/C", "ping -n 60 127.0.0.1 > NUL"]))
        .await
        .unwrap();
    let cmd_pid = p.pid().unwrap();
    let mut kids = Vec::new();
    for _ in 0..20 {
        kids = children_of(cmd_pid);
        if !kids.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let ping = *kids.first().expect("cmd never started ping");
    assert!(alive(ping));
    drop(p);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(!alive(ping), "grandchild {ping} survived the job object");
}

#[tokio::test]
async fn launch_of_missing_program_is_an_error() {
    let r = ProcessLauncher::default()
        .launch(spec(r"C:\definitely\not\here.exe", &[]))
        .await;
    assert!(r.is_err());
}

fn default_config() -> ModelConfig {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/default.toml");
    let mut table: toml::Table = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    table.remove("model").unwrap().try_into().unwrap()
}

/// Needs the GPU, the runtime and the weights from `docs/ops/setup.md`.
/// Run with `cargo nextest run -p nebula-model --run-ignored only`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the RTX 4070, llama-prism and the standard weights"]
async fn gpu_standard_chat_and_restart_after_kill() {
    let m = ModelManager::spawn(
        default_config(),
        SupervisorConfig::default(),
        Arc::new(ProcessLauncher::default()),
        None,
    )
    .unwrap();
    m.set_profile("standard").await.unwrap();
    let mut req = ChatRequest::new(vec![ChatMessage {
        role: Role::User,
        content: "Reply with the single word: ready".into(),
    }]);
    req.max_tokens = 32;
    let out = m
        .backend()
        .unwrap()
        .chat(req)
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    assert!(out.text.to_lowercase().contains("ready"), "{out:?}");
    assert!(out.usage.unwrap().predicted_n > 0);

    let pid = m.pid().unwrap();
    let killed = StdCommand::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let mut w = m.watch();
    tokio::time::timeout(
        Duration::from_secs(10),
        w.wait_for(|s| s.state == ModelState::Restarting),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(120),
        w.wait_for(|s| s.state == ModelState::Ready),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(m.status().restarts, 1);
    m.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs llama-stock and the embedding weights"]
async fn gpu_embedding_server_embeds() {
    let m = ModelManager::spawn(
        default_config(),
        SupervisorConfig::default(),
        Arc::new(ProcessLauncher::default()),
        None,
    )
    .unwrap();
    m.set_profile("embedding").await.unwrap();
    let v = m
        .backend()
        .unwrap()
        .embed(vec!["fn main() {}".into(), "hello".into()])
        .await
        .unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].len(), 1024);
    m.stop().await.unwrap();
}
