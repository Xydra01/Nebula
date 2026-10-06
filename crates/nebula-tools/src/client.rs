//! A minimal MCP client: newline-delimited JSON-RPC 2.0 over a child's stdin/stdout.
//!
//! Only what the tool host needs: the `initialize` handshake (plus the `notifications/initialized`
//! acknowledgement), `tools/list`, and `tools/call`. Requests are written as single JSON lines;
//! responses are read line by line and matched to their request by id.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::ToolError;
use crate::launcher::ChildIo;
use crate::types::{ToolCallResult, ToolDescriptor};

/// The MCP protocol version this client advertises in `initialize`.
const PROTOCOL_VERSION: &str = "2025-06-18";

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, ToolError>>>>>;

/// A connected tool server. Dropping it stops the reader task; the caller still owns the child
/// process (and its Job Object), which is what actually terminates the server.
pub struct McpClient {
    server: String,
    stdin: Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>,
    next_id: AtomicI64,
    pending: Pending,
    reader: JoinHandle<()>,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl McpClient {
    /// Takes a child's stdio and spawns the response reader. Does not perform the handshake;
    /// call [`McpClient::initialize`] next.
    #[must_use]
    pub fn new(server: String, io: ChildIo) -> Self {
        let ChildIo { stdin, stdout } = io;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader = tokio::spawn(read_loop(server.clone(), stdout, Arc::clone(&pending)));
        Self {
            server,
            stdin: Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            pending,
            reader,
        }
    }

    fn protocol_err(&self, detail: impl Into<String>) -> ToolError {
        ToolError::Protocol {
            server: self.server.clone(),
            detail: detail.into(),
        }
    }

    fn unavailable(&self, detail: impl Into<String>) -> ToolError {
        ToolError::Unavailable {
            server: self.server.clone(),
            detail: detail.into(),
        }
    }

    /// Runs the `initialize` request and sends `notifications/initialized`.
    ///
    /// # Errors
    /// [`ToolError::Unavailable`] or [`ToolError::Protocol`] if the server does not complete the
    /// handshake within `timeout`.
    pub async fn initialize(&self, timeout: Duration) -> Result<(), ToolError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "nebula", "version": env!("CARGO_PKG_VERSION") },
        });
        self.request("initialize", params, timeout).await?;
        self.notify("notifications/initialized", json!({})).await
    }

    /// Lists the server's tools, returning them tagged with this server's manifest name.
    ///
    /// # Errors
    /// [`ToolError::Unavailable`], [`ToolError::Protocol`], or a timeout.
    pub async fn list_tools(&self, timeout: Duration) -> Result<Vec<ToolDescriptor>, ToolError> {
        let result = self.request("tools/list", json!({}), timeout).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| self.protocol_err("tools/list result had no \"tools\" array"))?;
        tools
            .iter()
            .map(|t| self.parse_descriptor(t))
            .collect::<Result<Vec<_>, _>>()
    }

    fn parse_descriptor(&self, t: &Value) -> Result<ToolDescriptor, ToolError> {
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| self.protocol_err("a tool had no string \"name\""))?
            .to_owned();
        let description = t
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let input_schema = t
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" }));
        Ok(ToolDescriptor {
            server: self.server.clone(),
            name,
            description,
            input_schema,
        })
    }

    /// Calls `tool` with `arguments`. The caller is responsible for validating `arguments`
    /// against the tool schema first; this method only transports the call.
    ///
    /// # Errors
    /// [`ToolError::Unavailable`], [`ToolError::Protocol`], or a timeout.
    pub async fn call_tool(
        &self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<ToolCallResult, ToolError> {
        let params = json!({ "name": tool, "arguments": arguments });
        let result = self.request("tools/call", params, timeout).await?;
        let content = result
            .get("content")
            .cloned()
            .ok_or_else(|| self.protocol_err("tools/call result had no \"content\""))?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(ToolCallResult { content, is_error })
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ToolError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(e) = self.write_line(&line).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                // Sender dropped: the reader task ended, i.e. the server's stdout closed.
                Err(self.unavailable("server closed its output stream"))
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                // A timeout surfaces as Unavailable here; the host turns a *call* timeout into
                // ToolError::Timeout and kills the process.
                Err(self.unavailable(format!("no response to {method} within the timeout")))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), ToolError> {
        let line = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_line(&line).await
    }

    async fn write_line(&self, value: &Value) -> Result<(), ToolError> {
        let mut line = serde_json::to_string(value)
            .map_err(|e| self.protocol_err(format!("encoding request: {e}")))?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| self.unavailable(format!("writing to server stdin: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| self.unavailable(format!("flushing server stdin: {e}")))
    }
}

/// Reads JSON-RPC response lines and completes the matching pending request. Ends when the
/// server's stdout closes; any still-pending callers then see their `oneshot` sender dropped.
async fn read_loop(
    server: String,
    stdout: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    pending: Pending,
) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                dispatch(&server, trimmed, &pending).await;
            }
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(event = "tool.read_error", server = %server, error = %e);
                break;
            }
        }
    }
}

async fn dispatch(server: &str, line: &str, pending: &Pending) {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(event = "tool.bad_line", server = %server, error = %e);
            return;
        }
    };
    // Responses carry an id; notifications and requests from the server do not. We only act on
    // responses to our own requests.
    let Some(id) = msg.get("id").and_then(Value::as_i64) else {
        return;
    };
    let Some(tx) = pending.lock().await.remove(&id) else {
        return;
    };
    let outcome = if let Some(err) = msg.get("error") {
        let detail = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_owned();
        Err(ToolError::Protocol {
            server: server.to_owned(),
            detail,
        })
    } else {
        Ok(msg.get("result").cloned().unwrap_or(Value::Null))
    };
    let _ = tx.send(outcome);
}
