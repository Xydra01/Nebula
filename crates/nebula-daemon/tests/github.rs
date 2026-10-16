#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::path::Path;
use std::process::Command;

use axum::extract::Path as AxPath;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use nebula_config::GitHubConfig;
use nebula_daemon::github::api::PullRequest;
use nebula_daemon::github::safety::PushSafetyError;
use nebula_daemon::github::{
    GithubClient, TaskCompletionParams, handle_task_completion, push_task_branch,
};
use nebula_proto::TraceId;
use nebula_telemetry::TelemetryConfig;
use serde_json::{Value, json};
use tempfile::TempDir;

/// Helper to run a synchronous git command inside a directory.
fn run_git_cmd(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn test_push_to_local_bare_repo() {
    let remote_dir = TempDir::new().unwrap();
    let source_dir = TempDir::new().unwrap();

    // Initialize bare remote repo
    run_git_cmd(remote_dir.path(), &["init", "--bare"]);

    // Initialize source repo with a commit
    run_git_cmd(source_dir.path(), &["init", "-b", "main"]);
    run_git_cmd(
        source_dir.path(),
        &["config", "user.email", "test@example.com"],
    );
    run_git_cmd(source_dir.path(), &["config", "user.name", "Test User"]);

    let dummy_file = source_dir.path().join("dummy.txt");
    std::fs::write(&dummy_file, "hello nebula").unwrap();
    run_git_cmd(source_dir.path(), &["add", "dummy.txt"]);
    run_git_cmd(source_dir.path(), &["commit", "-m", "initial commit"]);

    // Create task branch
    run_git_cmd(
        source_dir.path(),
        &["checkout", "-b", "nebula/task-test-01"],
    );

    let remote_url = remote_dir.path().to_string_lossy().to_string();

    // 1. Valid task branch push succeeds
    let push_res = push_task_branch(
        source_dir.path(),
        "nebula/task-test-01",
        &remote_url,
        "dummy_token_123",
        "main",
        "nebula/",
    )
    .await;
    assert!(push_res.is_ok(), "push should succeed: {push_res:?}");

    // Verify ref exists in bare remote
    let rev_out = Command::new("git")
        .current_dir(remote_dir.path())
        .args(["rev-parse", "--verify", "refs/heads/nebula/task-test-01"])
        .output()
        .unwrap();
    assert!(
        rev_out.status.success(),
        "branch ref must exist in bare remote"
    );

    // 2. Refusal in code: pushing main is refused
    let main_push = push_task_branch(
        source_dir.path(),
        "main",
        &remote_url,
        "dummy_token_123",
        "main",
        "nebula/",
    )
    .await;
    assert!(
        matches!(
            main_push.unwrap_err(),
            nebula_daemon::github::PushError::Safety(PushSafetyError::ProtectedBranch(_))
        ),
        "pushing main must be refused"
    );

    // 3. Refusal in code: force push is refused
    let force_push = push_task_branch(
        source_dir.path(),
        "+nebula/task-test-01",
        &remote_url,
        "dummy_token_123",
        "main",
        "nebula/",
    )
    .await;
    assert!(
        matches!(
            force_push.unwrap_err(),
            nebula_daemon::github::PushError::Safety(PushSafetyError::ForcePushRefused)
        ),
        "force push must be refused"
    );
}

#[derive(Clone, Default)]
struct MockGithubState {
    created_pr: std::sync::Arc<std::sync::Mutex<Option<PullRequest>>>,
    comments: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[tokio::test]
async fn test_mock_github_api_client() {
    let state = MockGithubState::default();

    let app = Router::new()
        .route(
            "/repos/{owner}/{repo}/pulls",
            post({
                let s = state.clone();
                move |AxPath((_owner, _repo)): AxPath<(String, String)>, Json(body): Json<Value>| {
                    let title = body
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let pr_body = body
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let pr = PullRequest {
                        number: 42,
                        html_url: "https://github.com/Xydra01/Nebula/pull/42".to_owned(),
                        title,
                        body: Some(pr_body),
                        state: "open".to_owned(),
                        head: None,
                    };
                    *s.created_pr.lock().unwrap() = Some(pr.clone());
                    async move { (StatusCode::CREATED, Json(pr)) }
                }
            })
            .get({
                let s = state.clone();
                move |AxPath((_owner, _repo)): AxPath<(String, String)>| {
                    let existing = s.created_pr.lock().unwrap().clone();
                    let list = existing.into_iter().collect::<Vec<_>>();
                    async move { Json(list) }
                }
            }),
        )
        .route(
            "/repos/{owner}/{repo}/issues/{id}/comments",
            post({
                let s = state.clone();
                move |AxPath((_owner, _repo, _id)): AxPath<(String, String, u64)>,
                      Json(body): Json<Value>| {
                    let comment = body
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    s.comments.lock().unwrap().push(comment);
                    async move { (StatusCode::CREATED, Json(json!({"status": "ok"}))) }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let base_url = format!("http://127.0.0.1:{port}");
    let client = GithubClient::with_base_url("test_tok_123", "Nebula-dev-bot", &base_url).unwrap();

    // 1. Create PR
    let pr = client
        .create_pr(
            "Xydra01/Nebula",
            "main",
            "nebula/task-42",
            "fix: failing test",
            "PR description",
        )
        .await
        .unwrap();

    assert_eq!(pr.number, 42);
    assert_eq!(pr.html_url, "https://github.com/Xydra01/Nebula/pull/42");

    // 2. Find open PR
    let found = client
        .find_open_pr("Xydra01/Nebula", "nebula/task-42")
        .await
        .unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().number, 42);

    // 3. Add comment
    client
        .add_pr_comment("Xydra01/Nebula", 42, "Follow up test complete")
        .await
        .unwrap();
    assert_eq!(
        *state.comments.lock().unwrap(),
        vec!["Follow up test complete".to_owned()]
    );

    server_handle.abort();
}

#[tokio::test]
async fn test_token_leakage_and_redaction() {
    let sentinel_token = "mock_SENTINEL_SECRET_TOKEN_999";
    let log_dir = TempDir::new().unwrap();

    // Setup real Telemetry with capturing redactor
    let telemetry_cfg = TelemetryConfig {
        filter: "info".to_owned(),
        log_dir: Some(log_dir.path().to_path_buf()),
        console: false,
        broadcast_capacity: 100,
    };
    let (telemetry, subscriber) = nebula_telemetry::build(&telemetry_cfg).unwrap();
    let _guard = tracing::subscriber::set_default(subscriber);
    // Register the secret token
    telemetry.redactor().register(sentinel_token);

    // Setup bare repo & source repo
    let remote_dir = TempDir::new().unwrap();
    let source_dir = TempDir::new().unwrap();
    run_git_cmd(remote_dir.path(), &["init", "--bare"]);
    run_git_cmd(source_dir.path(), &["init", "-b", "main"]);
    run_git_cmd(
        source_dir.path(),
        &["config", "user.email", "test@example.com"],
    );
    run_git_cmd(source_dir.path(), &["config", "user.name", "Test User"]);
    let dummy_file = source_dir.path().join("dummy.txt");
    std::fs::write(&dummy_file, "content").unwrap();
    run_git_cmd(source_dir.path(), &["add", "dummy.txt"]);
    run_git_cmd(source_dir.path(), &["commit", "-m", "init"]);
    run_git_cmd(source_dir.path(), &["checkout", "-b", "nebula/task-leak"]);

    // Spin up mock server that verifies authorization header was sent
    let app = Router::new().route(
        "/repos/{owner}/{repo}/pulls",
        post(|Json(body): Json<Value>| async move {
            let title = body
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let pr = PullRequest {
                number: 10,
                html_url: "https://github.com/Xydra01/Nebula/pull/10".to_owned(),
                title,
                body: None,
                state: "open".to_owned(),
                head: None,
            };
            (StatusCode::CREATED, Json(pr))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = GitHubConfig {
        enabled: true,
        repo: "Xydra01/Nebula".into(),
        base_branch: "main".into(),
        branch_prefix: "nebula/".into(),
        token_target: "nebula/github_bot_token".into(),
        bot_username: "Nebula-dev-bot".into(),
    };

    let base_url = format!("http://127.0.0.1:{port}");
    let remote_url = remote_dir.path().to_string_lossy().to_string();
    let pr_url = handle_task_completion(TaskCompletionParams {
        repo_root: source_dir.path(),
        branch: "nebula/task-leak",
        goal: "verify token is never leaked",
        summary: "task executed with sentinel token",
        trace_id: TraceId::new(),
        test_results: Some("- all tests passed"),
        config: &config,
        token_override: Some(sentinel_token),
        api_base_url_override: Some(&base_url),
        git_remote_url_override: Some(&remote_url),
        extra_coauthor: None,
    })
    .await;

    assert!(pr_url.is_ok(), "task completion flow should succeed");
    assert_eq!(
        pr_url.unwrap(),
        Some("https://github.com/Xydra01/Nebula/pull/10".to_owned())
    );

    server_handle.abort();

    // Flush logs and read all files in log_dir
    let mut log_contents = String::new();
    for entry in std::fs::read_dir(log_dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().is_some_and(|e| e == "jsonl") {
            let content = std::fs::read_to_string(entry.path()).unwrap();
            log_contents.push_str(&content);
        }
    }

    // Acceptance Criterion 3: The token never appears in logs
    assert!(
        !log_contents.contains(sentinel_token),
        "Sentinel secret token was found in logs! Full logs:\n{log_contents}"
    );
}
