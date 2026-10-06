//! The tool host: one MCP client per configured server, a tool registry, argument validation,
//! and `tool.call` logging.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonschema::Validator;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::client::McpClient;
use crate::config::{ToolHostConfig, ToolServerConfig};
use crate::launcher::{LaunchSpec, StdioLauncher, ToolLauncher};
use crate::types::{ToolCallResult, ToolDescriptor};
use crate::{DEFAULT_STARTUP_TIMEOUT_MS, ToolError};

use nebula_telemetry::BlobStore;

/// One connected server: its client, the child handle that keeps it alive, and its limits.
struct Server {
    client: McpClient,
    child: Mutex<Box<dyn crate::launcher::ChildProcess>>,
    call_timeout: Duration,
    max_output_bytes: usize,
}

/// A registered tool: which server owns it and the compiled validator for its arguments.
struct Tool {
    server: String,
    descriptor: ToolDescriptor,
    validator: Arc<Validator>,
}

/// Hosts the configured MCP tool servers and routes calls to them.
///
/// Tool names are unique across servers; the first server to claim a name wins and later
/// duplicates are skipped with a warning.
pub struct ToolHost {
    servers: HashMap<String, Server>,
    tools: HashMap<String, Tool>,
    blobs: Option<BlobStore>,
}

impl ToolHost {
    /// Launches every server in `config` with the real [`StdioLauncher`], runs each handshake,
    /// and builds the tool registry. Servers that fail to start or list their tools are logged
    /// and skipped, so one broken server does not take down the host.
    ///
    /// # Errors
    /// Never fails as a whole; per-server failures are logged. Returns `Ok` with whatever
    /// started.
    pub async fn start(
        config: &ToolHostConfig,
        blobs: Option<BlobStore>,
    ) -> Result<Self, ToolError> {
        Self::start_with(config, blobs, &StdioLauncher).await
    }

    /// A host with no servers and no tools. Useful for the daemon when `[tools]` is empty and
    /// for callers that need a host handle without launching anything.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            servers: HashMap::new(),
            tools: HashMap::new(),
            blobs: None,
        }
    }

    /// [`ToolHost::start`] with an injected launcher, for tests.
    ///
    /// # Errors
    /// See [`ToolHost::start`].
    pub async fn start_with(
        config: &ToolHostConfig,
        blobs: Option<BlobStore>,
        launcher: &dyn ToolLauncher,
    ) -> Result<Self, ToolError> {
        let mut host = Self {
            servers: HashMap::new(),
            tools: HashMap::new(),
            blobs,
        };
        for (name, server_cfg) in &config.servers {
            if let Err(e) = host.add_server(name, server_cfg, launcher).await {
                tracing::warn!(event = "tool.server_start_failed", server = %name, error = %e);
            }
        }
        tracing::info!(
            event = "tool.host_ready",
            servers = host.servers.len(),
            tools = host.tools.len(),
        );
        Ok(host)
    }

    async fn add_server(
        &mut self,
        name: &str,
        cfg: &ToolServerConfig,
        launcher: &dyn ToolLauncher,
    ) -> Result<(), ToolError> {
        let spec = LaunchSpec {
            server: name.to_owned(),
            program: cfg.command.clone(),
            args: cfg.args.clone(),
            env: cfg.env.clone(),
        };
        let (child, io) = launcher.launch(spec).await?;
        let client = McpClient::new(name.to_owned(), io);
        let startup = Duration::from_millis(DEFAULT_STARTUP_TIMEOUT_MS);
        client.initialize(startup).await?;
        let descriptors = client.list_tools(startup).await?;

        self.servers.insert(
            name.to_owned(),
            Server {
                client,
                child: Mutex::new(child),
                call_timeout: Duration::from_millis(cfg.call_timeout_ms),
                max_output_bytes: cfg.max_output_bytes,
            },
        );
        for descriptor in descriptors {
            self.register_tool(name, descriptor);
        }
        Ok(())
    }

    fn register_tool(&mut self, server: &str, descriptor: ToolDescriptor) {
        if let Some(existing) = self.tools.get(&descriptor.name) {
            tracing::warn!(
                event = "tool.duplicate_name",
                tool = %descriptor.name,
                kept = %existing.server,
                skipped = %server,
            );
            return;
        }
        let validator = match jsonschema::validator_for(&descriptor.input_schema) {
            Ok(v) => Arc::new(v),
            Err(e) => {
                tracing::warn!(
                    event = "tool.bad_schema",
                    server = %server,
                    tool = %descriptor.name,
                    error = %e,
                );
                return;
            }
        };
        self.tools.insert(
            descriptor.name.clone(),
            Tool {
                server: server.to_owned(),
                descriptor,
                validator,
            },
        );
    }

    /// Every registered tool, sorted by name.
    #[must_use]
    pub fn list_tools(&self) -> Vec<ToolDescriptor> {
        let mut out: Vec<ToolDescriptor> =
            self.tools.values().map(|t| t.descriptor.clone()).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Validates `arguments` against the tool's schema, then calls it, enforcing the owning
    /// server's timeout and output cap. Emits a `tool.call` event tagged with `trace_id`.
    ///
    /// # Errors
    /// [`ToolError::UnknownTool`] if no server exposes it; [`ToolError::InvalidArguments`] if the
    /// arguments fail schema validation (the server is never contacted); [`ToolError::Timeout`]
    /// if the call runs past the server's timeout (the server is killed);
    /// [`ToolError::OutputTooLarge`] if the result exceeds the cap; or a transport/protocol error.
    pub async fn call(
        &self,
        tool: &str,
        arguments: Value,
        trace_id: Option<&str>,
    ) -> Result<ToolCallResult, ToolError> {
        let started = Instant::now();
        let result = self.call_inner(tool, &arguments, trace_id).await;
        self.log_call(tool, &arguments, &result, trace_id, started);
        result
    }

    async fn call_inner(
        &self,
        tool: &str,
        arguments: &Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolCallResult, ToolError> {
        let entry = self
            .tools
            .get(tool)
            .ok_or_else(|| ToolError::UnknownTool(tool.to_owned()))?;
        // Validate before touching the server, so bad arguments never reach it.
        if let Some(error) = entry.validator.iter_errors(arguments).next() {
            return Err(ToolError::InvalidArguments {
                tool: tool.to_owned(),
                detail: error.to_string(),
            });
        }
        let server = self
            .servers
            .get(&entry.server)
            .ok_or_else(|| ToolError::UnknownServer(entry.server.clone()))?;

        // The host owns the deadline so a hung tool is unambiguously a timeout (and the server
        // is killed), distinct from the server crashing, which surfaces as Unavailable. The
        // client keeps its own generous deadline only as a backstop against a lost response.
        let backstop = server.call_timeout.saturating_mul(2);
        let call = server.client.call_tool(tool, arguments.clone(), backstop);
        match tokio::time::timeout(server.call_timeout, call).await {
            Ok(Ok(result)) => self.enforce_cap(tool, result, server.max_output_bytes),
            Ok(Err(other)) => Err(other),
            Err(_) => {
                server.child.lock().await.stop().await;
                Err(ToolError::Timeout {
                    tool: tool.to_owned(),
                    timeout_ms: duration_ms(server.call_timeout),
                })
            }
        }
    }

    fn enforce_cap(
        &self,
        tool: &str,
        result: ToolCallResult,
        cap: usize,
    ) -> Result<ToolCallResult, ToolError> {
        let got = serde_json::to_vec(&result.content).map_or(0, |v| v.len());
        if got > cap {
            return Err(ToolError::OutputTooLarge {
                tool: tool.to_owned(),
                got,
                cap,
            });
        }
        Ok(result)
    }

    fn log_call(
        &self,
        tool: &str,
        arguments: &Value,
        result: &Result<ToolCallResult, ToolError>,
        trace_id: Option<&str>,
        started: Instant,
    ) {
        let put = |v: &Value| -> Option<String> {
            let blobs = self.blobs.as_ref()?;
            let bytes = serde_json::to_vec(v).ok()?;
            match blobs.put(&bytes) {
                Ok(r) => Some(r.to_string()),
                Err(e) => {
                    tracing::warn!(event = "blob.write_failed", error = %e);
                    None
                }
            }
        };
        let args_blob = put(arguments);
        let server = self.tools.get(tool).map(|t| t.server.as_str());
        let wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (outcome, output_blob, error) = match result {
            Ok(r) if r.is_error => ("tool_error", put(&r.content), None),
            Ok(r) => ("ok", put(&r.content), None),
            Err(e) => ("error", None, Some(e.to_string())),
        };
        tracing::info!(
            event = "tool.call",
            trace_id,
            server,
            tool,
            outcome,
            wall_ms,
            args_blob = args_blob.as_deref(),
            output_blob = output_blob.as_deref(),
            error = error.as_deref(),
        );
    }

    /// Stops every server (killing the child processes). Call during daemon shutdown.
    pub async fn stop(&self) {
        for (name, server) in &self.servers {
            server.child.lock().await.stop().await;
            tracing::debug!(event = "tool.server_stopped", server = %name);
        }
    }
}

fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
