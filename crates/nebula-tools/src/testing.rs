//! An in-process fake stdio MCP server and a launcher that connects to it, for tests in this
//! crate and its users (the daemon). Behind the `test-support` feature.
//!
//! The fake speaks the same newline-delimited JSON-RPC the real client expects, over a
//! `tokio::io::duplex` pair instead of a real pipe, so no child process is spawned.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

use crate::ToolError;
use crate::launcher::{ChildIo, ChildProcess, LaunchSpec, ToolLauncher};

/// One tool the fake server advertises and answers.
#[derive(Clone)]
pub struct FakeTool {
    /// Tool name.
    pub name: String,
    /// Its MCP `inputSchema`.
    pub input_schema: Value,
    /// The `content` value returned by `tools/call`.
    pub result: Value,
    /// Whether the result sets `isError`.
    pub is_error: bool,
    /// If true, the server reads the call but never replies, to exercise call timeouts.
    pub hang: bool,
}

impl FakeTool {
    /// A tool that echoes a required string `message` argument back as text content.
    #[must_use]
    pub fn echo() -> Self {
        Self {
            name: "echo".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"],
                "additionalProperties": false,
            }),
            result: json!([{ "type": "text", "text": "ok" }]),
            is_error: false,
            hang: false,
        }
    }

    /// A tool that never answers a call.
    #[must_use]
    pub fn hanging() -> Self {
        Self {
            name: "sleep".into(),
            input_schema: json!({ "type": "object" }),
            result: Value::Null,
            is_error: false,
            hang: true,
        }
    }
}

/// Builds a launcher serving `tools` from every server it starts.
#[must_use]
pub fn launcher(tools: Vec<FakeTool>) -> Arc<FakeLauncher> {
    Arc::new(FakeLauncher {
        tools,
        alive: Arc::new(AtomicBool::new(true)),
    })
}

/// A launcher that answers from an in-process fake server instead of spawning a process.
pub struct FakeLauncher {
    tools: Vec<FakeTool>,
    alive: Arc<AtomicBool>,
}

impl FakeLauncher {
    /// Whether the (single) fake child is still considered running.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ToolLauncher for FakeLauncher {
    async fn launch(
        &self,
        _spec: LaunchSpec,
    ) -> Result<(Box<dyn ChildProcess>, ChildIo), ToolError> {
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (server_rx, server_tx) = tokio::io::split(server_side);
        tokio::spawn(serve(self.tools.clone(), server_rx, server_tx));
        let (client_rx, client_tx) = tokio::io::split(client_side);
        let io = ChildIo {
            stdin: Box::new(client_tx),
            stdout: Box::new(client_rx),
        };
        let child = FakeChild {
            alive: Arc::clone(&self.alive),
        };
        Ok((Box::new(child), io))
    }
}

struct FakeChild {
    alive: Arc<AtomicBool>,
}

#[async_trait]
impl ChildProcess for FakeChild {
    fn pid(&self) -> Option<u32> {
        Some(4242)
    }
    fn try_exit(&mut self) -> Option<String> {
        (!self.alive.load(Ordering::SeqCst)).then(|| "killed".to_owned())
    }
    async fn stop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
    }
}

async fn serve(
    tools: Vec<FakeTool>,
    rx: tokio::io::ReadHalf<DuplexStream>,
    mut tx: tokio::io::WriteHalf<DuplexStream>,
) {
    let by_name: BTreeMap<String, FakeTool> =
        tools.into_iter().map(|t| (t.name.clone(), t)).collect();
    let mut lines = BufReader::new(rx).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        // Notifications (no id), e.g. notifications/initialized: nothing to answer.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let reply = match method {
            "initialize" => Some(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "fake", "version": "0.0.0" },
            })),
            "tools/list" => {
                let list: Vec<Value> = by_name
                    .values()
                    .map(|t| json!({ "name": t.name, "inputSchema": t.input_schema }))
                    .collect();
                Some(json!({ "tools": list }))
            }
            "tools/call" => {
                let name = msg
                    .get("params")
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match by_name.get(name) {
                    Some(t) if t.hang => continue, // read the call, never answer
                    Some(t) => Some(json!({ "content": t.result, "isError": t.is_error })),
                    None => None,
                }
            }
            _ => None,
        };
        let response = match reply {
            Some(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            None => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "method not found" },
            }),
        };
        let mut out = response.to_string();
        out.push('\n');
        if tx.write_all(out.as_bytes()).await.is_err() {
            break;
        }
        let _ = tx.flush().await;
    }
}

/// A minimal [`crate::config::ToolServerConfig`] pointing at a never-run command, with short
/// timeouts for tests. Pair it with [`launcher`].
#[must_use]
pub fn server_config(call_timeout_ms: u64) -> crate::config::ToolServerConfig {
    crate::config::ToolServerConfig {
        command: "fake-tool-server.exe".into(),
        args: Vec::new(),
        env: Vec::new(),
        call_timeout_ms,
        max_output_bytes: 1024 * 1024,
    }
}

/// A [`crate::config::ToolHostConfig`] with one server named `name`.
#[must_use]
pub fn host_config(name: &str, call_timeout_ms: u64) -> crate::config::ToolHostConfig {
    crate::config::ToolHostConfig {
        servers: BTreeMap::from([(name.to_owned(), server_config(call_timeout_ms))]),
    }
}
