//! In-process built-in tools and the shared types they produce.
//!
//! This feature (GitHub issue #27, design 6.1) adds the Rust, in-process half of the tool host
//! alongside the external stdio MCP servers from PR #42. Built-ins are called through the same
//! boundary as external tools: input-schema validation, per-call timeout, output-size cap, and a
//! single `tool.call` telemetry event.
//!
//! A built-in is an [`Arc<dyn BuiltinTool>`](BuiltinTool) stored on the registry entry; the host's
//! two-arm call path converges on the same validation / timeout / cap / logging path as external
//! tools. Everything a built-in needs that it must not reach for directly — the confinement root,
//! the command classifier, the resource snapshot — is injected through a [`ToolContext`], so the
//! not-yet-built crates (#28 permission engine, #29 per-task worktrees) can be swapped in without
//! touching the tools.

use std::sync::Arc;
use std::time::Duration;

use crate::ToolError;
use crate::permit::{CommandClassifier, Tier};

mod fs;
mod git;
mod resources;
mod shell;
mod task_job_seam;
mod worktree_provider;

pub use fs::{FsList, FsRead, FsSearch, FsWrite};
pub use git::{GitBranch, GitCommit, GitDiff, GitStatus};
pub use resources::SystemResources;
pub use shell::{ShellRun, scrub_env};
pub use task_job_seam::CURRENT_TASK_JOB;
#[cfg(windows)]
pub use task_job_seam::{JobChild, spawn_in_task_job};
pub use worktree_provider::{CURRENT_WORKTREE, TaskWorktreeProvider};

/// Raw output produced by a built-in tool, before the output cap is applied.
///
/// Carrying the payload as bytes lets the boundary measure and cap it uniformly in bytes,
/// independent of how a tool produced it (UTF-8 text for `fs`/`git`/`shell`, serialized JSON for
/// `system.resources`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    /// The full output as bytes, before any truncation.
    pub bytes: Vec<u8>,
    /// Whether the tool itself reported a (non-transport) error, mapped onto
    /// [`crate::types::ToolCallResult::is_error`].
    pub is_error: bool,
}

impl ToolOutput {
    /// Text output. `is_error` is `false`.
    #[must_use]
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            bytes: s.into().into_bytes(),
            is_error: false,
        }
    }

    /// JSON output (used by `system.resources`). `is_error` is `false`.
    ///
    /// # Errors
    /// [`ToolError::Protocol`] if `value` cannot be serialized.
    pub fn json(value: &serde_json::Value) -> Result<Self, ToolError> {
        let bytes = serde_json::to_vec(value).map_err(|e| ToolError::Protocol {
            server: "builtin".to_owned(),
            detail: format!("failed to serialize output: {e}"),
        })?;
        Ok(Self {
            bytes,
            is_error: false,
        })
    }
}

/// A Rust, in-process tool registered with the Tool Host.
///
/// Built-ins are called through the same boundary as external tools (schema validation, per-call
/// timeout, output cap, and the single `tool.call` telemetry event), so a built-in never bypasses
/// the host's safeguards.
///
/// The trait is object-safe (stored as `Arc<dyn BuiltinTool>` on the registry) and uses
/// [`async_trait`](async_trait::async_trait) for its async [`call`](BuiltinTool::call) method.
///
/// # Cancellation / drop-guard contract
///
/// A built-in's [`call`](BuiltinTool::call) future is wrapped in a per-call timeout. On timeout the
/// future is **dropped**, which is cooperative async cancellation — the built-in stops at its next
/// `.await`. Implementations MUST be cancellation-safe: any operating-system resource they own
/// (a child process in a Job Object, open file handles) must be released by a drop guard so that
/// dropping the future terminates the underlying work. Built-ins that do blocking work (the `git`
/// CLI, blocking file I/O) run it on a blocking task and hold a kill handle — the Job Object for a
/// process — so cancellation terminates the OS work; purely in-memory built-ins simply stop.
#[async_trait::async_trait]
pub trait BuiltinTool: Send + Sync {
    /// Stable tool name (1..=128 chars), unique across all built-in and external tools.
    ///
    /// The host rejects registration of a name outside that length or already held by another
    /// tool, and resolves `tools.call` by exact match on this value.
    fn name(&self) -> &str;

    /// Optional human-readable description surfaced in `tools.list`.
    ///
    /// Defaults to `None`.
    fn description(&self) -> Option<&str> {
        None
    }

    /// Non-empty JSON Schema for the tool's arguments.
    ///
    /// The host compiles this once at registration and validates every call's arguments against
    /// it before dispatching, so [`call`](BuiltinTool::call) may assume its arguments are valid.
    fn input_schema(&self) -> serde_json::Value;

    /// The permission [`Tier`] assigned to this tool at registration time.
    ///
    /// Tiers are fixed per tool (e.g. `fs.read` is [`Tier::Read`], `fs.write` is
    /// [`Tier::Sandbox`]) and are independent of any particular argument's validity.
    fn tier(&self) -> Tier;

    /// Run the tool.
    ///
    /// `arguments` have already passed schema validation. `ctx` supplies the injected boundaries
    /// (confinement root, classifier, resource snapshot, limits). See the
    /// [cancellation contract](Self#cancellation--drop-guard-contract): implementations MUST be
    /// cancellation-safe and release any OS resource via a drop guard when their future is dropped.
    ///
    /// # Errors
    /// Returns a [`ToolError`] when the tool itself cannot complete the request — for example
    /// [`ToolError::InvalidArguments`] for a confinement rejection or an unsupported action, or
    /// [`ToolError::Unavailable`] when a required resource (a repository, a snapshot) is absent.
    async fn call(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError>;
}

/// Supplies the directory that file and shell operations are confined to (design 7, the
/// `Worktree_Root`).
///
/// Defined as a trait so the daemon can inject the root from config today and issue #29 can later
/// supply a per-task worktree without changing any tool. The root may change between calls.
pub trait WorktreeRootProvider: Send + Sync {
    /// The current confinement root.
    ///
    /// May change between calls once issue #29 supplies per-task worktrees.
    fn worktree_root(&self) -> std::path::PathBuf;
}

/// Supplies the most recent resource snapshot.
///
/// Defined as a trait so `nebula-tools` does not depend on `nebula-resources`: the daemon wires
/// the real sampler, and `system.resources` returns the injected
/// [`nebula_proto::ResourceSnapshot`] unchanged (design 6).
pub trait ResourceProvider: Send + Sync {
    /// The latest resource snapshot, or `None` until the sampler has produced one.
    fn latest(&self) -> Option<nebula_proto::ResourceSnapshot>;
}

/// Per-call limits applied by the host to every built-in call.
///
/// Defaulted from config and clamped to the design's range (the call timeout to 1..=600 s) when
/// built by the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinLimits {
    /// Per-call timeout; the built-in's future is dropped (cancelled) when it elapses.
    pub call_timeout: Duration,
    /// Inline output cap in bytes; output beyond this is truncated and preserved in a blob.
    pub output_cap: usize,
}

/// Everything a built-in needs that it must not reach for directly.
///
/// Built by the daemon and shared across all built-ins. The provider handles are
/// [`Arc`]-wrapped so cloning a `ToolContext` is cheap (a few reference-count bumps), and
/// [`BuiltinLimits`] is `Copy`.
#[derive(Clone)]
pub struct ToolContext {
    /// Supplies the confinement root (issue #29 will provide the per-task implementation).
    pub worktree: Arc<dyn WorktreeRootProvider>,
    /// Classifies shell commands into a tier and approval decision (issue #28 full engine).
    pub classifier: Arc<dyn CommandClassifier>,
    /// Supplies the latest resource snapshot, or `None` if none has been taken yet.
    pub resources: Arc<dyn ResourceProvider>,
    /// The configured `resources.retired_drive` (default `"C:"`), passed to
    /// [`crate::path::resolve`] so every file/shell path check rejects the failing boot drive
    /// (design 7.6, AGENTS.md hard rule). Only its drive letter is compared, case-insensitively.
    ///
    /// Supplied by the daemon wiring (task 14.1); kept on the context rather than reached for
    /// directly so the resolver stays the single place confinement is decided (Requirement 7.1).
    pub retired_drive: String,
    /// Per-call timeout and output cap limits.
    pub limits: BuiltinLimits,
}

/// Register all ten built-in tools with the host in one call and install the context they are
/// called with.
///
/// Installs `ctx` via [`set_builtin_context`](crate::ToolHost::set_builtin_context), then registers
/// every built-in — `fs.read`, `fs.write`, `fs.list`, `fs.search`, `git.status`, `git.diff`,
/// `git.commit`, `git.branch`, `shell.run`, and `system.resources` — each at the tier it declares
/// through [`BuiltinTool::tier`]. Registration order does not matter: the host keys tools by name
/// and [`list_tools`](crate::ToolHost::list_tools) returns them sorted.
///
/// This is the single wiring point the daemon calls (`nebula_tools::builtins::register_all(&mut
/// host, ctx)`); because [`register_builtin`](crate::ToolHost::register_builtin) and
/// `set_builtin_context` both take `&mut self`, the host is fully configured before it is shared.
///
/// # Errors
/// Propagates the first [`register_builtin`](crate::ToolHost::register_builtin) error — an invalid
/// name or schema, or a name already held by another tool. The built-ins registered before the
/// failure remain on the host.
pub fn register_all(host: &mut crate::ToolHost, ctx: ToolContext) -> Result<(), ToolError> {
    host.set_builtin_context(ctx);
    host.register_builtin(Arc::new(FsRead))?;
    host.register_builtin(Arc::new(FsWrite))?;
    host.register_builtin(Arc::new(FsList))?;
    host.register_builtin(Arc::new(FsSearch))?;
    host.register_builtin(Arc::new(GitStatus))?;
    host.register_builtin(Arc::new(GitDiff))?;
    host.register_builtin(Arc::new(GitCommit))?;
    host.register_builtin(Arc::new(GitBranch))?;
    host.register_builtin(Arc::new(ShellRun))?;
    host.register_builtin(Arc::new(SystemResources))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BuiltinLimits, ResourceProvider, ToolContext, ToolOutput, WorktreeRootProvider,
        register_all,
    };
    use crate::ToolError;
    use crate::ToolHost;
    use crate::permit::{DefaultClassifier, Tier};
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    struct TestWorktree;

    impl WorktreeRootProvider for TestWorktree {
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

    fn test_ctx() -> ToolContext {
        ToolContext {
            worktree: Arc::new(TestWorktree),
            classifier: Arc::new(DefaultClassifier),
            resources: Arc::new(NoResources),
            retired_drive: "C:".to_owned(),
            limits: BuiltinLimits {
                call_timeout: Duration::from_secs(30),
                output_cap: 65_536,
            },
        }
    }

    #[test]
    fn register_all_registers_the_ten_builtins_with_declared_tiers() {
        let mut host = ToolHost::empty();
        register_all(&mut host, test_ctx()).expect("registering all built-ins should succeed");

        let names: Vec<String> = host.list_tools().into_iter().map(|d| d.name).collect();
        let expected = [
            "fs.list",
            "fs.read",
            "fs.search",
            "fs.write",
            "git.branch",
            "git.commit",
            "git.diff",
            "git.status",
            "shell.run",
            "system.resources",
        ];
        // `list_tools` sorts by name, so comparing against the sorted expected set checks both the
        // full membership (all ten) and that no extras slipped in.
        assert_eq!(names, expected);
    }

    #[test]
    fn each_builtin_declares_the_tier_from_the_design() {
        use super::{
            BuiltinTool, FsList, FsRead, FsSearch, FsWrite, GitBranch, GitCommit, GitDiff,
            GitStatus, ShellRun, SystemResources,
        };
        assert_eq!(FsRead.tier(), Tier::Read);
        assert_eq!(FsWrite.tier(), Tier::Sandbox);
        assert_eq!(FsList.tier(), Tier::Read);
        assert_eq!(FsSearch.tier(), Tier::Read);
        assert_eq!(GitStatus.tier(), Tier::Read);
        assert_eq!(GitDiff.tier(), Tier::Read);
        assert_eq!(GitCommit.tier(), Tier::Sandbox);
        assert_eq!(GitBranch.tier(), Tier::Sandbox);
        assert_eq!(ShellRun.tier(), Tier::Workspace);
        assert_eq!(SystemResources.tier(), Tier::Read);
    }

    #[test]
    fn text_produces_utf8_bytes_and_not_error() {
        let out = ToolOutput::text("hello world");
        assert_eq!(out.bytes, b"hello world".to_vec());
        assert!(!out.is_error);
    }

    #[test]
    fn text_preserves_exact_bytes_including_multibyte() {
        let s = "héllo — 日本語";
        let out = ToolOutput::text(s);
        assert_eq!(out.bytes, s.to_owned().into_bytes());
        assert!(!out.is_error);
    }

    #[test]
    fn text_empty_string_is_empty_bytes() {
        let out = ToolOutput::text("");
        assert!(out.bytes.is_empty());
        assert!(!out.is_error);
    }

    #[test]
    fn json_produces_serialized_bytes_and_not_error() {
        let value = json!({ "cpu": 0.5, "mem": [1, 2, 3], "name": "nebula" });
        let out = ToolOutput::json(&value).expect("serializing a Value should succeed");
        assert_eq!(
            out.bytes,
            serde_json::to_vec(&value).expect("reference serialization should succeed")
        );
        assert!(!out.is_error);
    }

    #[test]
    fn json_null_and_scalar_values_serialize() {
        for value in [json!(null), json!(42), json!("string"), json!(true)] {
            let out = ToolOutput::json(&value).expect("serializing a Value should succeed");
            assert_eq!(out.bytes, serde_json::to_vec(&value).expect("reference"));
            assert!(!out.is_error);
        }
    }

    // serde_json does not fail serializing an owned `Value` in practice, so the error path is
    // exercised by constructing the mapped error directly and asserting the variant/shape the
    // `json` helper produces on a serialization failure.
    #[test]
    fn json_serialization_error_maps_to_protocol() {
        let err = ToolError::Protocol {
            server: "builtin".to_owned(),
            detail: "failed to serialize output: boom".to_owned(),
        };
        match err {
            ToolError::Protocol { server, detail } => {
                assert_eq!(server, "builtin");
                assert!(detail.starts_with("failed to serialize output:"));
            }
            other => panic!("expected ToolError::Protocol, got {other:?}"),
        }
    }
}
