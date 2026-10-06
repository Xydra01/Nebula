//! Daemon-side implementations of the `nebula-tools` built-in provider traits.
//!
//! The built-in tools (`fs.*`, `git.*`, `shell.run`, `system.resources`) are called with a
//! [`ToolContext`](nebula_tools::ToolContext) that injects everything they must not reach for
//! directly: the confinement root, the command classifier, and the latest resource snapshot
//! (design 6.1, built-in tools wiring). The daemon owns these providers:
//!
//! - [`ConfigWorktreeRoot`] returns the configured `tools.builtin.worktree_root`. Issue #29 will
//!   replace it with a per-task worktree without touching any tool.
//! - [`SamplerResourceProvider`] reads the latest [`ResourceSnapshot`] from the very same
//!   [`watch`](tokio::sync::watch) channel the daemon samples into, so `system.resources` returns
//!   the exact value `resources.snapshot` returns for the same sampler state (Requirement 6.4).
//! - the command classifier is [`nebula_tools::DefaultClassifier`], used directly (issue #28
//!   replaces it with the real permission engine).
//!
//! The [`ToolContext`] is assembled in [`build_tool_context`] and installed on the host inside
//! [`crate::start`], after the sampler is created, so the resource provider is wired to the live
//! sampler before the host is shared.

use std::path::PathBuf;
use std::sync::Arc;

use nebula_config::NebulaConfig;
use nebula_proto::ResourceSnapshot;
use nebula_resources::Sampler;
use nebula_tools::{
    DefaultClassifier, ResourceProvider, ToolContext, ToolHost, WorktreeRootProvider,
};
use tokio::sync::watch;

/// Supplies the configured confinement root to the file and shell built-ins.
///
/// The root is `tools.builtin.worktree_root` from config, captured once at wiring time. Issue
/// #29 will supply a per-task worktree instead; the tools do not change.
struct ConfigWorktreeRoot {
    root: PathBuf,
}

impl WorktreeRootProvider for ConfigWorktreeRoot {
    fn worktree_root(&self) -> PathBuf {
        self.root.clone()
    }
}

/// Supplies `system.resources` with the latest snapshot, read from the same source as the
/// `resources.snapshot` RPC.
///
/// Holds a clone of the sampler's [`watch::Receiver`]. `borrow().clone()` reads the current
/// published snapshot — the identical value [`Sampler::latest`] (and therefore
/// [`crate::Shared::latest_snapshot`]) returns — which guarantees `system.resources` and
/// `resources.snapshot` agree field for field (Requirement 6.4). The receiver keeps the last
/// value readable even after the sampler stops, so a built-in call racing shutdown reads the
/// final snapshot rather than observing a transient `None`.
struct SamplerResourceProvider {
    rx: watch::Receiver<Option<ResourceSnapshot>>,
}

impl ResourceProvider for SamplerResourceProvider {
    fn latest(&self) -> Option<ResourceSnapshot> {
        self.rx.borrow().clone()
    }
}

/// A resource provider that always yields `None`, used when the sampler is disabled
/// (`deps.sources` is `None`).
///
/// This matches [`crate::Shared::latest_snapshot`], which returns `None` with no sampler, so
/// `system.resources` reports "no resource snapshot yet" just as `resources.snapshot` does.
struct NoResourceProvider;

impl ResourceProvider for NoResourceProvider {
    fn latest(&self) -> Option<ResourceSnapshot> {
        None
    }
}

/// Builds the [`ToolContext`] the built-in tools are called with.
///
/// The resource provider is wired to `sampler` when one is running (reading its live
/// [`watch`](tokio::sync::watch) channel) and to a `None`-yielding provider otherwise. The
/// classifier is the deterministic [`DefaultClassifier`]; the confinement root and retired drive
/// come from config, and the per-call limits are clamped by
/// [`BuiltinToolsConfig::limits`](nebula_tools::BuiltinToolsConfig::limits).
fn build_tool_context(config: &NebulaConfig, sampler: Option<&Sampler>) -> ToolContext {
    let resources: Arc<dyn ResourceProvider> = match sampler {
        Some(s) => Arc::new(SamplerResourceProvider { rx: s.subscribe() }),
        None => Arc::new(NoResourceProvider),
    };
    ToolContext {
        worktree: Arc::new(ConfigWorktreeRoot {
            root: config.tools.builtin.worktree_root.clone(),
        }),
        classifier: Arc::new(DefaultClassifier),
        resources,
        retired_drive: config.resources.retired_drive.clone(),
        limits: config.tools.builtin.limits(),
    }
}

/// Registers all built-in tools on `host`, wired with a [`ToolContext`] built from `config` and
/// the live `sampler`.
///
/// Called from [`crate::start`] on the still-exclusively-owned host, after the sampler is
/// created and before the host is wrapped in `Arc` and shared, because
/// [`register_all`](nebula_tools::builtins::register_all) and the context install both take
/// `&mut ToolHost`.
///
/// A registration failure is logged and the host is left with whatever registered before it; the
/// daemon still starts (built-ins are additive to the external tool host, which already tolerates
/// per-server failures).
pub(crate) fn register_builtins(
    host: &mut ToolHost,
    config: &NebulaConfig,
    sampler: Option<&Sampler>,
) {
    let ctx = build_tool_context(config, sampler);
    if let Err(e) = nebula_tools::builtins::register_all(host, ctx) {
        tracing::warn!(event = "tool.builtin_register_failed", error = %e);
    }
}
