//! The per-task confinement root — **relocated to `nebula-tools`** to avoid a dependency cycle.
//!
//! The git-worktree-per-task design (Section 4.4, Requirement 2) places the `CURRENT_WORKTREE`
//! task-local, the no-task-in-scope sentinel, and the `TaskWorktreeProvider` here, beside the
//! `WorktreeManager`. That is not possible: `TaskWorktreeProvider` must implement
//! `nebula_tools::WorktreeRootProvider`, and `nebula-tools` already depends on this crate (see the
//! crate doc, "Dependency direction"). Implementing the trait here would require this crate to
//! depend on `nebula-tools`, forming a cycle the crate forbids and the workspace cannot build.
//!
//! So the provider seam lives **with the trait**, in
//! [`nebula_tools::TaskWorktreeProvider`](../../../nebula_tools/struct.TaskWorktreeProvider.html)
//! and `nebula_tools::CURRENT_WORKTREE` (module `nebula_tools::builtins::worktree_provider`). This
//! divergence from the design keeps the infallible `worktree_root(&self) -> PathBuf` signature
//! intact so no issue #27 call site changes, and the daemon wiring (which depends on both crates)
//! installs the provider from there. The `WorktreeManager` lifecycle API still lives in this
//! crate's [`manager`](super::manager) module as the design intends.
//!
//! This module is intentionally empty; it is retained so the module tour in
//! [`super`] stays coherent and so this note is discoverable from the sandbox side.
