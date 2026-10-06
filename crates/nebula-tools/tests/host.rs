#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

//! Tool-host behaviour against an in-process fake MCP server (no child process spawned).

use std::time::Duration;

use nebula_tools::ToolError;
use nebula_tools::testing::{FakeTool, host_config, launcher};
use nebula_tools::{ToolCallResult, ToolHost};
use serde_json::json;

/// AC1: a server is launched, its tools are listed, and a call round-trips.
#[tokio::test]
async fn launch_list_and_call_round_trip() {
    let launcher = launcher(vec![FakeTool::echo()]);
    let host = ToolHost::start_with(&host_config("fs", 1_000), None, launcher.as_ref())
        .await
        .unwrap();

    let tools = host.list_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].server, "fs");

    let ToolCallResult { content, is_error } = host
        .call("echo", json!({ "message": "hi" }), Some("trace-1"))
        .await
        .unwrap();
    assert!(!is_error);
    assert_eq!(content, json!([{ "type": "text", "text": "ok" }]));
}

/// AC2: a call with invalid arguments is rejected before it reaches the server.
#[tokio::test]
async fn invalid_arguments_are_rejected_locally() {
    let launcher = launcher(vec![FakeTool::echo()]);
    let host = ToolHost::start_with(&host_config("fs", 1_000), None, launcher.as_ref())
        .await
        .unwrap();

    // `message` is required and must be a string; omit it.
    let err = host
        .call("echo", json!({ "wrong": 1 }), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::InvalidArguments { ref tool, .. } if tool == "echo"),
        "expected InvalidArguments, got {err:?}"
    );
}

/// Calling a tool no server exposes fails fast.
#[tokio::test]
async fn unknown_tool_is_rejected() {
    let launcher = launcher(vec![FakeTool::echo()]);
    let host = ToolHost::start_with(&host_config("fs", 1_000), None, launcher.as_ref())
        .await
        .unwrap();
    let err = host.call("nope", json!({}), None).await.unwrap_err();
    assert!(matches!(err, ToolError::UnknownTool(t) if t == "nope"));
}

/// AC3: a hung tool is killed at its timeout and the call returns an error, not a hang.
#[tokio::test(start_paused = true)]
async fn hung_tool_times_out_and_is_killed() {
    let launcher = launcher(vec![FakeTool::hanging()]);
    let host = ToolHost::start_with(&host_config("slow", 50), None, launcher.as_ref())
        .await
        .unwrap();
    assert!(launcher.alive());

    let call = host.call("sleep", json!({}), None);
    // With time paused, auto-advance fires the 50 ms call timeout deterministically.
    let err = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("the host must return rather than hang")
        .unwrap_err();

    assert!(
        matches!(err, ToolError::Timeout { ref tool, .. } if tool == "sleep"),
        "expected Timeout, got {err:?}"
    );
    assert!(
        !launcher.alive(),
        "the server process must have been killed"
    );
}

/// A tool whose result sets isError surfaces as a successful call with is_error = true.
#[tokio::test]
async fn tool_level_error_is_not_a_transport_error() {
    let mut tool = FakeTool::echo();
    tool.is_error = true;
    tool.result = json!([{ "type": "text", "text": "boom" }]);
    let launcher = launcher(vec![tool]);
    let host = ToolHost::start_with(&host_config("fs", 1_000), None, launcher.as_ref())
        .await
        .unwrap();

    let result = host
        .call("echo", json!({ "message": "x" }), None)
        .await
        .unwrap();
    assert!(result.is_error);
}
