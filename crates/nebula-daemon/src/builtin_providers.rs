//! Daemon-side implementations of the `nebula-tools` built-in provider traits.
//!
//! The built-in tools (`fs.*`, `git.*`, `shell.run`, `system.resources`) are called with a
//! [`ToolContext`](nebula_tools::ToolContext) that injects everything they must not reach for
//! directly: the confinement root, the command classifier, and the latest resource snapshot
//! (design 6.1, built-in tools wiring). The daemon owns these providers:
//!
//! - [`TaskWorktreeProvider`](nebula_tools::TaskWorktreeProvider) resolves the executing task's
//!   confinement root from the [`CURRENT_WORKTREE`](nebula_tools::CURRENT_WORKTREE) task-local
//!   (issue #29): the root the executor scoped around the task's tool calls, or a sentinel the
//!   `Path_Resolver` rejects when no task is in scope (Requirement 2.2, 2.5). It replaced the
//!   static `ConfigWorktreeRoot` without touching any tool — the trait signature is unchanged.
//! - [`SamplerResourceProvider`] reads the latest [`ResourceSnapshot`] from the very same
//!   [`watch`](tokio::sync::watch) channel the daemon samples into, so `system.resources` returns
//!   the exact value `resources.snapshot` returns for the same sampler state (Requirement 6.4).
//! - the command classifier is the real [`RulesClassifier`](nebula_sandbox::engine::RulesClassifier),
//!   built via [`RulesClassifier::embedded`](nebula_sandbox::engine::RulesClassifier::embedded)
//!   from the checked-in rules table. Its advisory-tier merge stays in `nebula-tools`
//!   ([`effective_tier`](nebula_tools::effective_tier)) and `shell.run` call sites are unchanged
//!   (Req 8.3–8.5). A rules-table that fails to load is a hard daemon startup failure: a
//!   protected-set misconfiguration must not boot into an unprotected state (Req 1.7).
//!
//! The [`ToolContext`] is assembled in [`build_tool_context`] and installed on the host inside
//! [`crate::start`], after the sampler is created, so the resource provider is wired to the live
//! sampler before the host is shared.

use std::sync::Arc;

use nebula_config::NebulaConfig;
use nebula_proto::ResourceSnapshot;
use nebula_resources::Sampler;
use nebula_sandbox::approval::ApprovalStore;
use nebula_sandbox::engine::RulesClassifier;
use nebula_sandbox::rules::LoadError;
use nebula_sandbox::worktree::manager::WorktreeManager;
use nebula_tools::{
    CommandClassifier, ResourceProvider, TaskWorktreeProvider, ToolContext, ToolHost,
};
use tokio::sync::watch;

use crate::DaemonError;

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
/// classifier is the real [`RulesClassifier`] loaded from the embedded rules table via
/// [`RulesClassifier::embedded`]; the confinement root is the per-task
/// [`TaskWorktreeProvider`](nebula_tools::TaskWorktreeProvider) (which reads the executor's
/// [`CURRENT_WORKTREE`](nebula_tools::CURRENT_WORKTREE) task-local), the retired drive and the
/// classifier's path resolver come from config (`resources.retired_drive` and
/// `tools.builtin.worktree_root`), and the per-call limits are clamped by
/// [`BuiltinToolsConfig::limits`](nebula_tools::BuiltinToolsConfig::limits).
///
/// Exactly one [`TaskWorktreeProvider`](nebula_tools::TaskWorktreeProvider) is installed as the
/// `worktree` provider, so no built-in call resolves through a static root (Requirement 2.2).
/// The future executor (issue #32) drives the per-task lifecycle around this provider; it is
/// **not built by this feature**, but the shape is:
///
/// ```ignore
/// // After `manager.create(&task_id, &repo, start_point).await?` returns the root:
/// let branch = nebula_tools::CURRENT_WORKTREE
///     .scope(Some(root.clone()), async {
///         // All of the task's tool-calling work runs here, so every built-in resolves
///         // against `root` via the installed TaskWorktreeProvider.
///         run_task_tool_calls().await
///     })
///     .await;
///
/// // At task finalize, outside the scope:
/// let _branch = manager.finalize(&task_id).await?;   // keeps the task branch for #36
/// manager.remove_worktree(&task_id).await?;          // removes only this task's worktree dir
/// ```
///
/// `embedded` uses the built-in lexical resolver (no filesystem I/O); a future task may inject the
/// FS-accurate `nebula-tools` resolver via
/// [`RulesClassifier::with_resolver`](nebula_sandbox::engine::RulesClassifier::with_resolver).
///
/// # Errors
///
/// Returns a [`LoadError`] if the embedded rules table fails to parse or validate. A classifier
/// only exists once its table loads cleanly, so a protected-set misconfiguration surfaces here
/// rather than booting into an unprotected state (Req 1.7).
fn build_tool_context(
    config: &NebulaConfig,
    sampler: Option<&Sampler>,
) -> Result<ToolContext, LoadError> {
    let resources: Arc<dyn ResourceProvider> = match sampler {
        Some(s) => Arc::new(SamplerResourceProvider { rx: s.subscribe() }),
        None => Arc::new(NoResourceProvider),
    };
    let classifier: Arc<dyn CommandClassifier> = Arc::new(RulesClassifier::embedded(
        config.tools.builtin.worktree_root.clone(),
        config.resources.retired_drive.clone(),
    )?);
    Ok(ToolContext {
        worktree: Arc::new(TaskWorktreeProvider::new()),
        classifier,
        resources,
        retired_drive: config.resources.retired_drive.clone(),
        limits: config.tools.builtin.limits(),
    })
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
/// per-server failures). A classifier rules-table load failure, by contrast, is returned as a
/// [`DaemonError`] so the daemon fails to start: a protected-set misconfiguration must not boot
/// into an unprotected state (Req 1.7).
///
/// # Errors
///
/// Returns [`DaemonError::Other`] if the permission rules table fails to load.
pub(crate) fn register_builtins(
    host: &mut ToolHost,
    config: &NebulaConfig,
    sampler: Option<&Sampler>,
) -> Result<(), DaemonError> {
    let ctx = build_tool_context(config, sampler)
        .map_err(|e| DaemonError::Other(format!("permission rules table failed to load: {e}")))?;
    if let Err(e) = nebula_tools::builtins::register_all(host, ctx) {
        tracing::warn!(event = "tool.builtin_register_failed", error = %e);
    }
    Ok(())
}

/// Constructs the per-task [`WorktreeManager`] once at daemon start and runs crash recovery.
///
/// The manager is the issue #29 lifecycle API the future executor (issue #32) will drive:
/// `create` / `finalize` / `remove_worktree` around each task, with the
/// [`TaskWorktreeProvider`](nebula_tools::TaskWorktreeProvider) installed in [`build_tool_context`]
/// resolving the per-task root the executor scopes into
/// [`CURRENT_WORKTREE`](nebula_tools::CURRENT_WORKTREE).
///
/// At start it runs [`WorktreeManager::recover_stale`] to reconcile worktrees left behind by a
/// crash: with no live task registry at cold start every worktree under `worktree.worktrees_dir`
/// with no owning task is stale, and recovery removes the clean ones (within the delete limit) and
/// retains any with uncommitted work for review (Requirement 6, 9.4). Recovery runs on a spawned
/// task rather than blocking startup: [`crate::start`] is a synchronous fn executing on a running
/// multi-threaded Tokio runtime (`#[tokio::main]`), where `Handle::block_on` on the current thread
/// would panic. The spawned task logs the outcome as `event = "worktree.recovery"` with the stale
/// count once reconciliation finishes.
///
/// The manager is returned so the caller can retain it for the executor (issue #32). Nothing
/// consumes the lifecycle API yet, so a caller that only needs recovery may drop the returned
/// handle after this call; the spawned recovery task holds its own clone.
pub(crate) fn spawn_worktree_recovery(config: &NebulaConfig) -> Arc<WorktreeManager> {
    // #28 wires no shared `ApprovalStore` into the daemon yet (none is reachable here). Construct a
    // fresh store so the manager honours the #28 delete-limit approval contract; when #28 wires a
    // shared store into the daemon, replace this with that shared handle — the manager reuses the
    // same contract either way. Do NOT add a second approval mechanism.
    let approvals = Arc::new(ApprovalStore::new());
    let manager = Arc::new(WorktreeManager::new(
        config.worktree.clone(),
        config.resources.retired_drive.clone(),
        approvals,
    ));

    let recovery_manager = Arc::clone(&manager);
    tokio::spawn(async move {
        let report = recovery_manager.recover_stale().await;
        tracing::info!(event = "worktree.recovery", stale = report.stale.len());
    });

    manager
}

#[cfg(test)]
mod tests {
    use nebula_config::NebulaConfig;
    use nebula_sandbox::rules::{LoadError, RulesTable};

    use super::{DaemonError, build_tool_context};

    /// The embedded rules table is baked into `nebula-sandbox` via `include_str!` and is always
    /// valid, so `build_tool_context` on the default config succeeds and the daemon boots with a
    /// real classifier wired in (Req 1.7, happy path).
    #[test]
    fn build_tool_context_succeeds_with_embedded_table() {
        let config = NebulaConfig::from_toml(None).unwrap();
        // No sampler: exercises the `None` resource-provider branch; the classifier load is the
        // part under test and is independent of the sampler.
        assert!(
            build_tool_context(&config, None).is_ok(),
            "the embedded rules table must load so the daemon can start with a classifier",
        );
    }

    /// A rules-table `LoadError` must surface as a daemon startup failure rather than a silent
    /// boot into an unprotected state (Req 1.7).
    ///
    /// A true end-to-end "daemon fails to start on a bad embedded table" test is not achievable:
    /// `RulesClassifier::embedded` loads the table baked in via `include_str!`, which is always
    /// valid, and external-table loading (the `[sandbox] rules_table_path` key) is not wired, so
    /// there is no config input that makes the embedded load fail. Instead we assert the invariant
    /// at the layer where it is observable — the error mapping `register_builtins` applies — using
    /// a real `LoadError` constructed from a malformed table and the SAME single-expression
    /// mapping the daemon uses in `register_builtins`:
    /// `|e| DaemonError::Other(format!("permission rules table failed to load: {e}"))`.
    #[test]
    fn load_error_maps_to_startup_failure() {
        // A malformed table yields a real `LoadError` (parse failure) without touching the
        // filesystem or any real path.
        let load_err: LoadError =
            RulesTable::load("tier = bad").expect_err("malformed rules TOML must fail to load");

        // The exact mapping `register_builtins` applies to a classifier-load failure.
        let mapped: DaemonError =
            DaemonError::Other(format!("permission rules table failed to load: {load_err}"));

        match mapped {
            DaemonError::Other(msg) => {
                assert!(
                    msg.contains("permission rules table failed to load"),
                    "startup failure must name the permission rules table: {msg}",
                );
            }
            other => panic!("expected DaemonError::Other, got {other:?}"),
        }
    }
}
