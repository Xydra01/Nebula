//! The tool host: one MCP client per configured server, a tool registry, argument validation,
//! and `tool.call` logging.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonschema::Validator;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::builtins::{BuiltinTool, ToolContext, ToolOutput};
use crate::cap::enforce_cap;
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

/// How a registered tool is dispatched.
///
/// External tools are routed to the owning MCP server over its stdio transport; built-ins are
/// called in-process through their [`BuiltinTool`] implementation. Both kinds share the same
/// registry entry so argument validation, the output cap, and the single `tool.call` event apply
/// uniformly.
enum ToolKind {
    /// A tool exposed by an external MCP server, dispatched to that server by name.
    External {
        /// The server (manifest key) that owns this tool.
        server: String,
    },
    /// A Rust, in-process built-in, called directly through its trait object.
    Builtin(Arc<dyn BuiltinTool>),
}

/// A registered tool: how it is dispatched, its advertised descriptor, and the compiled validator
/// for its arguments.
struct Tool {
    kind: ToolKind,
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
    /// The injected boundaries ([`ToolContext`]) built-ins are called with: the confinement root,
    /// command classifier, resource snapshot source, retired drive, and per-call limits.
    ///
    /// Stored at registration time (the daemon builds it and calls
    /// [`set_builtin_context`](ToolHost::set_builtin_context) alongside
    /// [`register_builtin`](ToolHost::register_builtin), both of which take `&mut self`). It is
    /// `None` on a host that has only external tools; a built-in call with no context set resolves
    /// to [`ToolError::Unavailable`] rather than panicking.
    builtin_ctx: Option<ToolContext>,
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
            builtin_ctx: None,
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
            builtin_ctx: None,
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
            // One bad tool (duplicate name or uncompilable schema) must not take down an
            // otherwise-healthy server: surface the per-tool error, log it, and skip just that
            // tool. The server and its other tools stay registered.
            if let Err(e) = self.register_tool(name, descriptor) {
                tracing::warn!(event = "tool.register_failed", server = %name, error = %e);
            }
        }
        Ok(())
    }

    /// Register an external tool exposed by `server`.
    ///
    /// # Errors
    /// [`ToolError::DuplicateTool`] if `descriptor.name` is already held by a built-in or external
    /// tool (the existing registration is left intact); [`ToolError::InvalidArguments`] if the
    /// input schema does not compile. On either error the tool is not registered.
    fn register_tool(&mut self, server: &str, descriptor: ToolDescriptor) -> Result<(), ToolError> {
        if self.tools.contains_key(&descriptor.name) {
            return Err(ToolError::DuplicateTool {
                name: descriptor.name,
            });
        }
        let validator = jsonschema::validator_for(&descriptor.input_schema).map_err(|e| {
            ToolError::InvalidArguments {
                tool: descriptor.name.clone(),
                detail: e.to_string(),
            }
        })?;
        self.tools.insert(
            descriptor.name.clone(),
            Tool {
                kind: ToolKind::External {
                    server: server.to_owned(),
                },
                descriptor,
                validator: Arc::new(validator),
            },
        );
        Ok(())
    }

    /// Register an in-process built-in tool.
    ///
    /// Builds a [`ToolDescriptor`] from the tool's `name`/`description`/`input_schema` (with
    /// `server = "builtin"`), validates the name and schema, compiles the argument validator, and
    /// inserts the tool so it is resolvable by its exact [`name`](BuiltinTool::name).
    ///
    /// # Errors
    /// [`ToolError::InvalidArguments`] if the name is empty or longer than 128 characters, or the
    /// input schema is empty or does not compile (Req 1.2); [`ToolError::DuplicateTool`] if the
    /// name is already held by a built-in or external tool, leaving the existing registration
    /// intact (Req 1.12). On either error the tool is not registered.
    pub fn register_builtin(&mut self, tool: Arc<dyn BuiltinTool>) -> Result<(), ToolError> {
        let name = tool.name().to_owned();
        // Name length 1..=128 (Req 1.1/1.2). Count chars, not bytes, since the bound is on the
        // name's characters.
        let name_len = name.chars().count();
        if !(1..=128).contains(&name_len) {
            return Err(ToolError::InvalidArguments {
                tool: name,
                detail: "tool name must be 1 to 128 characters".to_owned(),
            });
        }
        let input_schema = tool.input_schema();
        // A non-empty input schema is required (Req 1.2): reject an empty object schema (and the
        // other empty JSON shapes) before trying to compile it.
        if schema_is_empty(&input_schema) {
            return Err(ToolError::InvalidArguments {
                tool: name,
                detail: "input schema must not be empty".to_owned(),
            });
        }
        // Collision across BOTH maps leaves the existing registration intact (Req 1.12).
        if self.tools.contains_key(&name) {
            return Err(ToolError::DuplicateTool { name });
        }
        let validator =
            jsonschema::validator_for(&input_schema).map_err(|e| ToolError::InvalidArguments {
                tool: name.clone(),
                detail: e.to_string(),
            })?;
        let descriptor = ToolDescriptor {
            server: "builtin".to_owned(),
            name: name.clone(),
            description: tool.description().map(ToOwned::to_owned),
            input_schema,
        };
        self.tools.insert(
            name,
            Tool {
                kind: ToolKind::Builtin(tool),
                descriptor,
                validator: Arc::new(validator),
            },
        );
        Ok(())
    }

    /// Install the [`ToolContext`] that built-ins are called with.
    ///
    /// The daemon builds the context (confinement root, classifier, resource provider, retired
    /// drive, and per-call limits) from config and installs it alongside registering the
    /// built-ins — both this and [`register_builtin`](ToolHost::register_builtin) take `&mut self`,
    /// so the host is fully configured before it is wrapped in an `Arc` and shared. Calling it
    /// again replaces the stored context (the latest wins), which lets issue #29 swap in a
    /// per-task worktree later.
    pub fn set_builtin_context(&mut self, ctx: ToolContext) {
        self.builtin_ctx = Some(ctx);
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
        let outcome = self.call_inner(tool, &arguments, trace_id).await;
        // `call_inner` returns the full-output blob ref (when the output was capped) alongside the
        // result, so `log_call` records that blob without re-`put`ting the inline content.
        let (result, output_blob) = match outcome {
            Ok((result, blob)) => (Ok(result), blob),
            Err(e) => (Err(e), None),
        };
        self.log_call(tool, &arguments, &result, output_blob, trace_id, started);
        result
    }

    async fn call_inner(
        &self,
        tool: &str,
        arguments: &Value,
        _trace_id: Option<&str>,
    ) -> Result<(ToolCallResult, Option<String>), ToolError> {
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
        // Branch on how the tool is dispatched. Argument validation above is shared by both arms;
        // from here the external arm routes to the owning MCP server and the built-in arm calls the
        // in-process trait object. Both converge on the shared output cap and the single
        // `log_call` site in `call`.
        match &entry.kind {
            ToolKind::External {
                server: server_name,
            } => {
                let server = self
                    .servers
                    .get(server_name)
                    .ok_or_else(|| ToolError::UnknownServer(server_name.clone()))?;

                // The host owns the deadline so a hung tool is unambiguously a timeout (and the
                // server is killed), distinct from the server crashing, which surfaces as
                // Unavailable. The client keeps its own generous deadline only as a backstop
                // against a lost response.
                let backstop = server.call_timeout.saturating_mul(2);
                let call = server.client.call_tool(tool, arguments.clone(), backstop);
                match tokio::time::timeout(server.call_timeout, call).await {
                    // Route external results through the shared output cap: oversized output is
                    // truncated with a marker and the full output preserved in a blob, rather than
                    // rejected. This is a behavior change from PR #42's reject-on-cap (design 5).
                    // Within-cap results keep their original MCP content unchanged (Req 5.2); only
                    // over-cap results are reshaped into the truncated text block.
                    Ok(Ok(result)) => {
                        let output = result_to_output(&result);
                        if output.bytes.len() <= server.max_output_bytes {
                            Ok((result, None))
                        } else {
                            enforce_cap(tool, output, server.max_output_bytes, self.blobs.as_ref())
                        }
                    }
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
            ToolKind::Builtin(builtin) => {
                // Built-ins need the injected boundaries (confinement root, classifier, resource
                // snapshot, limits). The daemon installs these via `set_builtin_context` when it
                // registers the built-ins; a host that only ever registered externals has none, so
                // a built-in call there is Unavailable rather than a panic.
                let ctx = self
                    .builtin_ctx
                    .as_ref()
                    .ok_or_else(|| ToolError::Unavailable {
                        server: "builtin".to_owned(),
                        detail: "built-in tool context not configured".to_owned(),
                    })?;

                // The host owns the per-call deadline. On elapse the future is dropped, which is
                // cooperative async cancellation: a built-in's drop guard releases any OS resource
                // it holds (a child in a Job Object, open handles), so dropping terminates the
                // underlying work (see the BuiltinTool cancellation contract).
                let call = builtin.call(arguments.clone(), ctx);
                match tokio::time::timeout(ctx.limits.call_timeout, call).await {
                    // Route built-in output through the same byte-measured cap as external results:
                    // within-cap output is returned whole with no blob; over-cap output is
                    // truncated with a marker and the full output preserved in a blob.
                    Ok(Ok(output)) => {
                        enforce_cap(tool, output, ctx.limits.output_cap, self.blobs.as_ref())
                    }
                    Ok(Err(other)) => Err(other),
                    Err(_) => Err(ToolError::Timeout {
                        tool: tool.to_owned(),
                        timeout_ms: duration_ms(ctx.limits.call_timeout),
                    }),
                }
            }
        }
    }

    fn log_call(
        &self,
        tool: &str,
        arguments: &Value,
        result: &Result<ToolCallResult, ToolError>,
        output_blob: Option<String>,
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
        let server = self.tools.get(tool).map(tool_owner);
        let wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        // `output_blob` is the full-output blob computed by `enforce_cap` when the output was
        // capped; it is already the preserved full output, so it is not re-`put` here. For
        // uncapped results it is `None` and no output blob is recorded.
        let (outcome, output_blob, error) = match result {
            Ok(r) if r.is_error => ("tool_error", output_blob, None),
            Ok(_) => ("ok", output_blob, None),
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

/// Whether a built-in's declared input schema is "empty" and so should be rejected (Req 1.2).
///
/// A schema is empty when it is JSON `null`, an empty object `{}`, an empty array `[]`, or an
/// empty string. Any object or array with members is treated as non-empty; schema *validity* is
/// then decided by compiling it with `jsonschema`.
fn schema_is_empty(schema: &Value) -> bool {
    match schema {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::String(s) => s.is_empty(),
        _ => false,
    }
}

/// The server name used to label a tool in logs: the owning MCP server for an external tool, or
/// the constant `"builtin"` for an in-process built-in.
fn tool_owner(tool: &Tool) -> &str {
    match &tool.kind {
        ToolKind::External { server } => server.as_str(),
        ToolKind::Builtin(_) => "builtin",
    }
}

/// Convert an external tool's [`ToolCallResult`] into a [`ToolOutput`] so it passes through the
/// same byte-measured output cap as built-ins. The MCP `content` is serialized to bytes for
/// measurement; its `is_error` flag is preserved.
fn result_to_output(result: &ToolCallResult) -> ToolOutput {
    let bytes = serde_json::to_vec(&result.content).unwrap_or_default();
    ToolOutput {
        bytes,
        is_error: result.is_error,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    // `super::*` brings in the host-internal items the tests rely on (notably the private
    // `schema_is_empty` used by Property 9, plus `ToolHost`, `ToolError`, `Arc`, `Value`, and the
    // `BuiltinTool`/`ToolContext`/`ToolOutput` imports already in scope at module level).
    use super::*;
    use crate::builtins::{BuiltinLimits, ResourceProvider, WorktreeRootProvider};
    use crate::permit::{DefaultClassifier, Tier};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// A trivial in-crate built-in used to prove the external/builtin registry boundary before any
    /// real tool exists: it echoes a required `message` string and records whether its `call` was
    /// ever reached, so a test can assert that invalid arguments are rejected *before* dispatch.
    struct FakeEcho {
        /// Flipped to `true` the moment `call` runs; stays `false` if validation rejects first.
        invoked: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl BuiltinTool for FakeEcho {
        fn name(&self) -> &str {
            "fake.echo"
        }

        fn description(&self) -> Option<&str> {
            Some("echoes its message back (test fake)")
        }

        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"],
                "additionalProperties": false
            })
        }

        fn tier(&self) -> Tier {
            Tier::Read
        }

        async fn call(
            &self,
            arguments: Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            self.invoked.store(true, Ordering::SeqCst);
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(ToolOutput::text(message.to_owned()))
        }
    }

    struct FakeWorktree;

    impl WorktreeRootProvider for FakeWorktree {
        fn worktree_root(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }
    }

    struct NoResources;

    impl ResourceProvider for NoResources {
        fn latest(&self) -> Option<nebula_proto::ResourceSnapshot> {
            None
        }
    }

    /// A minimal [`ToolContext`] for exercising the builtin arm: a tempdir confinement root, a
    /// resource provider that yields nothing, the default classifier, the retired drive, and a
    /// short-ish per-call timeout with the 64 KiB output cap.
    fn test_ctx() -> ToolContext {
        ToolContext {
            worktree: Arc::new(FakeWorktree),
            classifier: Arc::new(DefaultClassifier),
            resources: Arc::new(NoResources),
            retired_drive: "C:".to_owned(),
            limits: BuiltinLimits {
                call_timeout: Duration::from_secs(5),
                output_cap: 65_536,
            },
        }
    }

    /// A host with the fake built-in registered and its context installed, plus the invocation
    /// flag the fake flips when its `call` runs.
    fn host_with_fake() -> (ToolHost, Arc<AtomicBool>) {
        let invoked = Arc::new(AtomicBool::new(false));
        let mut host = ToolHost::empty();
        host.set_builtin_context(test_ctx());
        host.register_builtin(Arc::new(FakeEcho {
            invoked: Arc::clone(&invoked),
        }))
        .expect("registering the fake built-in should succeed");
        (host, invoked)
    }

    /// Req 1.3: a registered built-in appears in `list_tools` with the `"builtin"` server label and
    /// the exact input schema it declared.
    #[tokio::test]
    async fn builtin_appears_in_list_tools_with_its_schema() {
        let (host, _invoked) = host_with_fake();

        let tools = host.list_tools();
        let echo = tools
            .iter()
            .find(|d| d.name == "fake.echo")
            .expect("fake.echo must be listed");
        assert_eq!(echo.server, "builtin");
        assert_eq!(
            echo.description.as_deref(),
            Some("echoes its message back (test fake)")
        );
        assert_eq!(
            echo.input_schema,
            json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"],
                "additionalProperties": false
            })
        );
    }

    /// Req 1.4: a built-in is callable through the host and its output round-trips.
    #[tokio::test]
    async fn builtin_is_callable_and_echoes_its_input() {
        let (host, invoked) = host_with_fake();

        let result = host
            .call("fake.echo", json!({ "message": "hi" }), Some("trace-1"))
            .await
            .expect("calling the built-in should succeed");

        assert!(!result.is_error);
        let text = result.content[0]["text"]
            .as_str()
            .expect("built-in text output is wrapped in an MCP text block");
        assert_eq!(text, "hi");
        assert!(
            invoked.load(Ordering::SeqCst),
            "the built-in's call must have run"
        );
    }

    /// Req 1.5: arguments are validated against the schema before dispatch — a missing required
    /// field returns `InvalidArguments` and the built-in's `call` is never reached.
    #[tokio::test]
    async fn invalid_arguments_are_rejected_before_the_builtin_runs() {
        let (host, invoked) = host_with_fake();

        let err = host
            .call("fake.echo", json!({}), None)
            .await
            .expect_err("missing the required `message` field must be rejected");

        assert!(
            matches!(err, ToolError::InvalidArguments { ref tool, .. } if tool == "fake.echo"),
            "expected InvalidArguments, got {err:?}"
        );
        assert!(
            !invoked.load(Ordering::SeqCst),
            "validation must reject before the built-in's call runs"
        );
    }

    /// Req 1.11: a name held by neither map returns `UnknownTool`.
    #[tokio::test]
    async fn unknown_name_returns_unknown_tool() {
        let (host, invoked) = host_with_fake();

        let err = host
            .call("does.not.exist", json!({}), None)
            .await
            .expect_err("an unregistered name must be rejected");

        assert!(
            matches!(err, ToolError::UnknownTool(ref name) if name == "does.not.exist"),
            "expected UnknownTool, got {err:?}"
        );
        assert!(
            !invoked.load(Ordering::SeqCst),
            "an unknown name must not reach any built-in"
        );
    }

    /// A built-in whose `name` and `input_schema` are set per test case, so the registration
    /// validation can be exercised across the full matrix of valid and invalid names and schemas.
    /// Everything else is trivial: a `Read` tier and a no-op `call` returning empty text. It is
    /// never actually invoked in these tests; only registration is checked.
    struct ConfigurableTool {
        name: String,
        schema: Value,
    }

    #[async_trait::async_trait]
    impl BuiltinTool for ConfigurableTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn input_schema(&self) -> Value {
            self.schema.clone()
        }

        fn tier(&self) -> Tier {
            Tier::Read
        }

        async fn call(
            &self,
            _arguments: Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text(""))
        }
    }

    // Feature: builtin-tools, Property 9: registration validation accepts valid tools and rejects
    // invalid ones
    //
    // Property 9: Registration validation accepts valid tools and rejects invalid ones. For any
    // generated tool name (valid, empty, or over-length) and input schema (non-empty+compilable,
    // empty, or uncompilable), `register_builtin` on a fresh host accepts the tool exactly when
    // the name is 1..=128 chars AND the schema is non-empty AND it compiles — then the tool is
    // resolvable by exact name and listed, and the registry grew by one. Otherwise it returns an
    // error (InvalidArguments, since a fresh host has no collision) and the registry is unchanged.
    // The expected-validity oracle mirrors the implementation (`schema_is_empty`: null / `{}` /
    // `[]` / `""` are empty; validity is then `jsonschema::validator_for`). See design.md,
    // Property 9 (Validates: Requirements 1.1, 1.2, 1.3).
    mod property_registration_validation {
        use super::*;
        // The host-private `schema_is_empty` oracle: a glob `use super::*` does not re-export the
        // names `super` itself glob-imported, so reach for it by its canonical path.
        use crate::host::schema_is_empty;
        use proptest::prelude::*;

        /// Generate names across the three classes the validator cares about: valid
        /// (1..=128 chars), empty (0 chars → invalid), and over-length (129..300 chars →
        /// invalid). Weighted toward valid so accepted registrations are well exercised.
        fn any_name() -> impl Strategy<Value = String> {
            prop_oneof![
                2 => "[a-z][a-z0-9_.]{0,40}",
                1 => Just(String::new()),
                1 => "[a-z]{129,300}",
            ]
        }

        /// Generate schemas across valid (non-empty + compilable), empty (rejected before
        /// compiling), and uncompilable (non-empty but rejected by `jsonschema`) classes.
        fn any_schema() -> impl Strategy<Value = Value> {
            prop_oneof![
                // Valid: a non-empty, compilable object schema.
                Just(json!({
                    "type": "object",
                    "properties": { "x": { "type": "string" } },
                    "additionalProperties": false,
                })),
                // Empty shapes rejected by `schema_is_empty` before compilation.
                Just(json!({})),
                Just(json!(null)),
                Just(json!([])),
                Just(json!("")),
                // Non-empty but uncompilable: `type` must be a string/array of strings, not a
                // number, so `jsonschema::validator_for` rejects it.
                Just(json!({ "type": 123 })),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn valid_registered_invalid_rejected_registry_unchanged(
                name in any_name(),
                schema in any_schema(),
            ) {
                let mut host = ToolHost::empty();
                let before = host.list_tools().len();
                prop_assert_eq!(before, 0, "fresh host must start empty");

                // Oracle mirroring the implementation's three checks.
                let name_valid = (1..=128).contains(&name.chars().count());
                let schema_nonempty = !schema_is_empty(&schema);
                let schema_compilable = jsonschema::validator_for(&schema).is_ok();
                let valid = name_valid && schema_nonempty && schema_compilable;

                let tool = Arc::new(ConfigurableTool {
                    name: name.clone(),
                    schema: schema.clone(),
                });
                let result = host.register_builtin(tool);

                let after = host.list_tools();
                let listed = after.iter().any(|d| d.name == name);

                if valid {
                    prop_assert!(
                        result.is_ok(),
                        "valid tool rejected: name={name:?} schema={schema:?} err={result:?}",
                    );
                    // Resolvable by exact name and listed.
                    prop_assert!(listed, "accepted tool {name:?} not listed");
                    prop_assert_eq!(after.len(), before + 1, "registry did not grow by one");
                } else {
                    // A fresh host has no collision, so an invalid registration is always
                    // InvalidArguments.
                    match &result {
                        Err(ToolError::InvalidArguments { .. }) => {}
                        other => prop_assert!(
                            false,
                            "invalid tool not rejected with InvalidArguments: \
                             name={name:?} schema={schema:?} result={other:?}",
                        ),
                    }
                    // Registry unchanged: same length and the name is absent.
                    prop_assert_eq!(after.len(), before, "registry changed on rejected tool");
                    prop_assert!(!listed, "rejected tool {name:?} leaked into the registry");
                }
            }
        }
    }

    // Feature: builtin-tools, Property 10: tool names are unique across both maps
    //
    // Validates: Requirements 1.11, 1.12
    //
    // Over pre-populated registries of distinct built-in names: re-registering any already-held
    // name returns `DuplicateTool` with the existing registration intact and the tool count
    // unchanged; a candidate name held by neither map makes `call` return `UnknownTool`, invoking
    // nothing. Built-in and external tools share the one `tools` map keyed by name, so the
    // duplicate check is identical across kinds; the dedicated `#[tokio::test]` below proves the
    // external->built-in direction explicitly through a real external tool.
    mod property_name_uniqueness {
        use super::{ConfigurableTool, test_ctx};
        use std::collections::HashSet;
        use std::sync::Arc;

        use proptest::prelude::*;
        use serde_json::json;

        use crate::ToolError;
        use crate::host::ToolHost;

        /// A pre-populated built-in whose name is chosen per case; a valid non-empty schema so
        /// registration validation always accepts it (name uniqueness, not schema validity, is
        /// what this property exercises).
        fn builtin(name: impl Into<String>) -> Arc<ConfigurableTool> {
            Arc::new(ConfigurableTool {
                name: name.into(),
                schema: json!({ "type": "object", "additionalProperties": true }),
            })
        }

        /// A set of 1..6 distinct valid built-in names to pre-register, plus a candidate name that
        /// is either drawn from that set (a guaranteed in-set duplicate) or generated fresh (which
        /// might still coincide with a member, so the assertion branches on actual membership).
        fn names_and_candidate() -> impl Strategy<Value = (Vec<String>, String)> {
            proptest::collection::hash_set("[a-z][a-z0-9_.]{0,20}", 1..6).prop_flat_map(|set| {
                let names: Vec<String> = set.into_iter().collect();
                let from_set = (0..names.len()).prop_map({
                    let names = names.clone();
                    move |i| names[i].clone()
                });
                let fresh = "[a-z][a-z0-9_.]{0,20}";
                let candidate = prop_oneof![from_set, fresh];
                (Just(names), candidate)
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(200))]

            #[test]
            fn held_names_dup_and_fresh_names_unknown(
                (names, candidate) in names_and_candidate(),
            ) {
                // A current-thread runtime so the async `call` resolves inside the sync proptest
                // closure without needing a multi-thread executor.
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("current-thread runtime");

                rt.block_on(async {
                    let mut host = ToolHost::empty();
                    host.set_builtin_context(test_ctx());
                    for name in &names {
                        host.register_builtin(builtin(name.clone()))
                            .expect("registering a distinct valid name should succeed");
                    }

                    let before = host.list_tools();
                    let held: HashSet<&str> = before.iter().map(|d| d.name.as_str()).collect();

                    if held.contains(candidate.as_str()) {
                        // Re-registering a held name -> DuplicateTool naming that name, with the
                        // existing registration intact and the tool count unchanged (Req 1.12).
                        let err = host
                            .register_builtin(builtin(candidate.clone()))
                            .expect_err("re-registering a held name must be a duplicate");
                        prop_assert!(
                            matches!(err, ToolError::DuplicateTool { ref name } if *name == candidate),
                            "expected DuplicateTool({candidate:?}), got {err:?}"
                        );
                        let after = host.list_tools();
                        prop_assert_eq!(after.len(), before.len(), "tool count must be unchanged");
                        prop_assert!(
                            after.iter().any(|d| d.name == candidate),
                            "the original registration for {candidate:?} must remain"
                        );
                    } else {
                        // A name held by neither map -> `call` returns UnknownTool, invoking
                        // nothing (Req 1.11).
                        let err = host
                            .call(&candidate, json!({}), None)
                            .await
                            .expect_err("calling an unregistered name must fail");
                        prop_assert!(
                            matches!(err, ToolError::UnknownTool(ref t) if *t == candidate),
                            "expected UnknownTool({candidate:?}), got {err:?}"
                        );
                    }
                    Ok(())
                })?;
            }
        }
    }

    /// Cross-map collision (Req 1.12): a built-in whose name is already held by an EXTERNAL tool is
    /// rejected with `DuplicateTool`, and the external registration stays intact and callable.
    ///
    /// This complements the property above (which pre-populates built-ins) by exercising the
    /// external -> built-in direction through a real external tool from the fake launcher.
    #[tokio::test]
    async fn builtin_colliding_with_external_name_is_a_duplicate() {
        use crate::testing::{FakeTool, host_config, launcher};

        let launcher = launcher(vec![FakeTool::echo()]);
        let mut host = ToolHost::start_with(&host_config("fs", 1_000), None, launcher.as_ref())
            .await
            .expect("fake host starts");
        host.set_builtin_context(test_ctx());

        // "echo" is the external tool advertised by the fake server; a built-in with the same
        // name must collide across the shared map.
        let collider = Arc::new(ConfigurableTool {
            name: "echo".to_owned(),
            schema: json!({ "type": "object", "additionalProperties": true }),
        });
        let err = host
            .register_builtin(collider)
            .expect_err("a built-in named like the external tool must be a duplicate");
        assert!(
            matches!(err, ToolError::DuplicateTool { ref name } if name == "echo"),
            "expected DuplicateTool(\"echo\"), got {err:?}"
        );

        // The external registration is intact: still exactly one tool, still owned by the external
        // server (not relabelled "builtin"), and it still round-trips a call.
        let tools = host.list_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(
            tools[0].server, "fs",
            "the external registration must remain"
        );

        let result = host
            .call("echo", json!({ "message": "hi" }), None)
            .await
            .expect("the external echo tool must still be callable");
        assert!(!result.is_error);
        assert_eq!(result.content, json!([{ "type": "text", "text": "ok" }]));
    }

    // Feature: builtin-tools, Property 5: exactly one tool.call event per invocation
    //
    // Validates: Requirements 1.10, 8.1, 8.2, 8.4, 8.6
    //
    // Every `ToolHost::call`, whatever the outcome, funnels through the single `log_call` site that
    // wraps `call_inner`, so each invocation emits exactly one `tool.call` tracing event carrying
    // the passed trace id, a non-empty `outcome`, and the elapsed `wall_ms`. This property drives a
    // configurable fake built-in (and host-level paths that never reach a built-in) across the full
    // set of outcome classes — success, tool error, host- and tool-level InvalidArguments, host-
    // and tool-level Timeout, Unavailable, OutputTooLarge, and UnknownTool — and, for each, scopes
    // a capturing subscriber to that one call and asserts exactly one matching `tool.call` event
    // (Req 1.10, 8.1, 8.2, 8.4, 8.6).
    mod property_one_event_per_call {
        use super::{BuiltinLimits, ResourceProvider, ToolContext, WorktreeRootProvider};
        use crate::ToolError;
        use crate::builtins::{BuiltinTool, ToolOutput};
        use crate::host::ToolHost;
        use crate::permit::{DefaultClassifier, Tier};

        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use proptest::prelude::*;
        use serde_json::{Value, json};
        use tracing::field::{Field, Visit};
        use tracing::subscriber::with_default;
        use tracing_subscriber::layer::{Context, SubscriberExt};
        use tracing_subscriber::{Layer, Registry};

        /// The subset of a captured `tool.call` event the property asserts on: the structured
        /// `trace_id`, `outcome`, and `wall_ms` fields recorded by `log_call`.
        #[derive(Clone, Debug, Default)]
        struct CapturedCall {
            trace_id: Option<String>,
            outcome: Option<String>,
            wall_ms: Option<u64>,
        }

        /// Pulls the `event`, `trace_id`, `outcome`, and `wall_ms` fields off a `tool.call` event.
        #[derive(Default)]
        struct CallVisitor {
            event: Option<String>,
            captured: CapturedCall,
        }

        impl Visit for CallVisitor {
            fn record_u64(&mut self, field: &Field, value: u64) {
                if field.name() == "wall_ms" {
                    self.captured.wall_ms = Some(value);
                }
            }

            fn record_str(&mut self, field: &Field, value: &str) {
                match field.name() {
                    "event" => self.event = Some(value.to_owned()),
                    "trace_id" => self.captured.trace_id = Some(value.to_owned()),
                    "outcome" => self.captured.outcome = Some(value.to_owned()),
                    _ => {}
                }
            }

            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                // `trace_id` arrives as `Option<&str>`, which `tracing` records through its
                // `Debug` path (e.g. `Some("abc")`), not `record_str`. `outcome` is a `&'static
                // str` recorded via `record_str`; `event` likewise. Parse the `Some("…")` /
                // `None` debug shape for the fields that come through here.
                let name = field.name();
                if name != "trace_id" && name != "outcome" && name != "event" {
                    return;
                }
                let rendered = format!("{value:?}");
                let parsed = parse_debug_opt_str(&rendered);
                match name {
                    "event" => self.event = parsed.or(self.event.take()),
                    "trace_id" => self.captured.trace_id = parsed,
                    "outcome" => self.captured.outcome = parsed.or(self.captured.outcome.take()),
                    _ => {}
                }
            }
        }

        /// Interpret a `tracing` `Debug`-rendered optional/plain string field. `Some("x")` and
        /// `None` (the `Option<&str>` shapes used for `trace_id`) map to `Some("x")`/`None`; a bare
        /// quoted string `"x"` maps to `Some("x")`; anything else is returned verbatim as `Some`.
        fn parse_debug_opt_str(rendered: &str) -> Option<String> {
            let s = rendered.trim();
            if s == "None" {
                return None;
            }
            if let Some(inner) = s.strip_prefix("Some(").and_then(|r| r.strip_suffix(')')) {
                return Some(unquote(inner.trim()));
            }
            Some(unquote(s))
        }

        /// Strip a single pair of surrounding double quotes if present.
        fn unquote(s: &str) -> String {
            s.strip_prefix('"')
                .and_then(|r| r.strip_suffix('"'))
                .unwrap_or(s)
                .to_owned()
        }

        /// A `tracing` layer that records every `tool.call` event into a shared vector, so a single
        /// scoped `call` can be asserted to emit exactly one.
        struct CaptureLayer {
            calls: Arc<Mutex<Vec<CapturedCall>>>,
        }

        impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                let mut visitor = CallVisitor::default();
                event.record(&mut visitor);
                if visitor.event.as_deref() == Some("tool.call")
                    && let Ok(mut calls) = self.calls.lock()
                {
                    calls.push(visitor.captured);
                }
            }
        }

        struct FakeWorktree;

        impl WorktreeRootProvider for FakeWorktree {
            fn worktree_root(&self) -> std::path::PathBuf {
                std::env::temp_dir()
            }
        }

        struct NoResources;

        impl ResourceProvider for NoResources {
            fn latest(&self) -> Option<nebula_proto::ResourceSnapshot> {
                None
            }
        }

        /// A context whose per-call timeout is tiny, so the `HostTimeout` case elapses quickly, and
        /// whose output cap is small, so the `OutputTooLarge` case trips it with a modest payload.
        fn ctx_for_property() -> ToolContext {
            ToolContext {
                worktree: Arc::new(FakeWorktree),
                classifier: Arc::new(DefaultClassifier),
                resources: Arc::new(NoResources),
                retired_drive: "C:".to_owned(),
                limits: BuiltinLimits {
                    call_timeout: Duration::from_millis(50),
                    output_cap: 64,
                },
            }
        }

        /// What a single invocation should drive. Each variant maps to one outcome class the
        /// `tool.call` event records; the fake built-in produces the tool-level ones, and the host
        /// produces the host-level ones (schema rejection, timeout, unknown name) before or around
        /// dispatch.
        #[derive(Clone, Debug)]
        enum OutcomeClass {
            /// `Ok(text)` — recorded `outcome = "ok"`.
            Success,
            /// `Ok(ToolOutput { is_error: true, .. })` — recorded `outcome = "tool_error"`.
            ToolError,
            /// The built-in returns `Err(InvalidArguments)` — recorded `outcome = "error"`.
            BuiltinInvalidArguments,
            /// The built-in returns `Err(Timeout)` — recorded `outcome = "error"`.
            BuiltinTimeout,
            /// The built-in returns `Err(Unavailable)` — recorded `outcome = "error"`.
            Unavailable,
            /// The built-in returns output over the cap with no blob store, so the host maps it to
            /// `OutputTooLarge` — recorded `outcome = "error"`.
            OutputTooLarge,
            /// The arguments fail schema validation before dispatch — recorded `outcome =
            /// "error"`, the built-in never runs.
            HostInvalidArguments,
            /// The built-in sleeps past the tiny per-call timeout, so the host returns `Timeout` —
            /// recorded `outcome = "error"`.
            HostTimeout,
            /// A name held by neither map — recorded `outcome = "error"`, nothing is invoked.
            UnknownTool,
        }

        fn any_outcome_class() -> impl Strategy<Value = OutcomeClass> {
            prop_oneof![
                Just(OutcomeClass::Success),
                Just(OutcomeClass::ToolError),
                Just(OutcomeClass::BuiltinInvalidArguments),
                Just(OutcomeClass::BuiltinTimeout),
                Just(OutcomeClass::Unavailable),
                Just(OutcomeClass::OutputTooLarge),
                Just(OutcomeClass::HostInvalidArguments),
                Just(OutcomeClass::HostTimeout),
                Just(OutcomeClass::UnknownTool),
            ]
        }

        /// A built-in whose single `call` yields the outcome configured for the case.
        struct ConfigurableOutcome {
            class: OutcomeClass,
            cap: usize,
        }

        #[async_trait::async_trait]
        impl BuiltinTool for ConfigurableOutcome {
            fn name(&self) -> &str {
                "fake.outcome"
            }

            fn input_schema(&self) -> Value {
                json!({
                    "type": "object",
                    "properties": { "message": { "type": "string" } },
                    "required": ["message"],
                    "additionalProperties": false
                })
            }

            fn tier(&self) -> Tier {
                Tier::Read
            }

            async fn call(
                &self,
                _arguments: Value,
                _ctx: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                match self.class {
                    OutcomeClass::Success => Ok(ToolOutput::text("x")),
                    OutcomeClass::ToolError => Ok(ToolOutput {
                        bytes: b"boom".to_vec(),
                        is_error: true,
                    }),
                    OutcomeClass::BuiltinInvalidArguments => Err(ToolError::InvalidArguments {
                        tool: "fake.outcome".to_owned(),
                        detail: "rejected by the tool".to_owned(),
                    }),
                    OutcomeClass::BuiltinTimeout => Err(ToolError::Timeout {
                        tool: "fake.outcome".to_owned(),
                        timeout_ms: 1,
                    }),
                    OutcomeClass::Unavailable => Err(ToolError::Unavailable {
                        server: "builtin".to_owned(),
                        detail: "nothing here".to_owned(),
                    }),
                    OutcomeClass::OutputTooLarge => {
                        // Over the cap with no blob store on the host -> OutputTooLarge.
                        Ok(ToolOutput::text("y".repeat(self.cap + 1)))
                    }
                    OutcomeClass::HostTimeout => {
                        // Sleep well past the tiny per-call timeout so the host cancels and reports
                        // Timeout; the sleep is cancellation-safe (no OS resource held).
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                        Ok(ToolOutput::text("unreachable"))
                    }
                    // These two never reach the built-in: the host rejects the arguments
                    // (HostInvalidArguments) or resolves no tool at all (UnknownTool) before
                    // dispatch. Returning success here is harmless because `call` is never run.
                    OutcomeClass::HostInvalidArguments | OutcomeClass::UnknownTool => {
                        Ok(ToolOutput::text("unreachable"))
                    }
                }
            }
        }

        /// The outcome string `log_call` records for a given class.
        fn expected_outcome(class: &OutcomeClass) -> &'static str {
            match class {
                OutcomeClass::Success => "ok",
                OutcomeClass::ToolError => "tool_error",
                _ => "error",
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(135))]

            #[test]
            fn exactly_one_tool_call_event_per_invocation(
                class in any_outcome_class(),
                // A non-empty, printable trace id; the event must carry exactly this value.
                trace_id in "[A-Za-z0-9][A-Za-z0-9._:-]{0,40}",
            ) {
                let cap = ctx_for_property().limits.output_cap;

                // A fresh host per case with NO blob store, so the OutputTooLarge path trips when
                // the fake returns over-cap output. The context installs the tiny timeout/cap.
                let mut host = ToolHost::empty();
                host.set_builtin_context(ctx_for_property());

                // Every class but UnknownTool registers the fake under its name; UnknownTool leaves
                // the registry empty so the call resolves to no tool.
                let registered_name = "fake.outcome";
                if !matches!(class, OutcomeClass::UnknownTool) {
                    host.register_builtin(Arc::new(ConfigurableOutcome {
                        class: class.clone(),
                        cap,
                    }))
                    .expect("registering the configurable fake must succeed");
                }

                // The tool to call and the arguments differ only for the two host-level paths:
                // UnknownTool calls a name held by neither map; HostInvalidArguments sends
                // arguments that fail the schema (missing the required `message`). Everything else
                // calls the registered fake with valid arguments.
                let (tool, args): (&str, Value) = match class {
                    OutcomeClass::UnknownTool => ("not.registered", json!({ "message": "hi" })),
                    OutcomeClass::HostInvalidArguments => (registered_name, json!({})),
                    _ => (registered_name, json!({ "message": "hi" })),
                };

                let calls: Arc<Mutex<Vec<CapturedCall>>> = Arc::new(Mutex::new(Vec::new()));
                let layer = CaptureLayer {
                    calls: Arc::clone(&calls),
                };
                let subscriber = Registry::default().with(layer);

                // Scope the capturing subscriber to this single invocation so the captured events
                // belong to exactly one `call`. `call` is async, so run it on a current-thread
                // runtime's `block_on` INSIDE the `with_default` scope — the events are emitted on
                // this thread while the scoped subscriber is the default.
                let trace = trace_id.clone();
                with_default(subscriber, || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("current-thread runtime");
                    rt.block_on(async {
                        let _ = host.call(tool, args.clone(), Some(trace.as_str())).await;
                    });
                });

                let captured = calls.lock().expect("capture mutex not poisoned").clone();

                // Exactly one `tool.call` event per invocation (Req 1.10, 8.1).
                prop_assert_eq!(
                    captured.len(),
                    1,
                    "expected exactly one tool.call event for {:?}, got {}",
                    class,
                    captured.len()
                );
                let event = &captured[0];

                // It carries the passed trace id (Req 8.2, 8.6).
                prop_assert_eq!(
                    event.trace_id.as_deref(),
                    Some(trace_id.as_str()),
                    "event must carry the passed trace id for {:?}",
                    class
                );

                // It records a non-empty outcome, matching the class (Req 8.4).
                let outcome = event.outcome.as_deref().unwrap_or_default();
                prop_assert!(!outcome.is_empty(), "outcome must be non-empty for {:?}", class);
                prop_assert_eq!(
                    outcome,
                    expected_outcome(&class),
                    "outcome mismatch for {:?}",
                    class
                );

                // It records the elapsed duration (Req 8.4). `wall_ms` is a u64, so present-and-
                // >= 0 is just present.
                prop_assert!(
                    event.wall_ms.is_some(),
                    "event must record wall_ms for {:?}",
                    class
                );
            }
        }
    }
}
