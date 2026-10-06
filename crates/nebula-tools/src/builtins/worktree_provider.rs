//! The per-task confinement root: the `CURRENT_WORKTREE` task-local, the no-task-in-scope
//! sentinel, and the [`TaskWorktreeProvider`] the daemon installs in place of
//! `ConfigWorktreeRoot` (GitHub issue #29, design Section 4.4, Requirement 2).
//!
//! # Why this lives in `nebula-tools`, not `nebula-sandbox`
//!
//! The git-worktree-per-task design places this provider in `nebula-sandbox::worktree::provider`
//! so it sits beside the `WorktreeManager`. That is **not possible without a dependency cycle**:
//! [`WorktreeRootProvider`](crate::builtins::WorktreeRootProvider) is defined here in
//! `nebula-tools`, and `nebula-tools` already depends on `nebula-sandbox` (and re-exports its
//! permission vocabulary). Implementing the trait inside `nebula-sandbox` would require
//! `nebula-sandbox` → `nebula-tools`, which the `nebula-sandbox` crate doc explicitly forbids
//! ("this crate must **not** depend on `nebula-tools`, to avoid a cycle") and which would break
//! the workspace build.
//!
//! So the type that implements `WorktreeRootProvider` lives **with the trait**, in `nebula-tools`.
//! The `WorktreeManager` lifecycle API still lives in `nebula-sandbox::worktree` as the design
//! intends; only this provider seam (the trait implementor and the task-local it reads) is placed
//! on the `nebula-tools` side of the dependency edge. This is the least-churn way to keep the
//! infallible `worktree_root(&self) -> PathBuf` signature intact so **no issue #27 call site
//! changes**, and the daemon wiring (task 8.1, `nebula-daemon` — which depends on both crates) can
//! still reach it as `nebula_tools::TaskWorktreeProvider`.

use std::path::PathBuf;

use crate::builtins::WorktreeRootProvider;

tokio::task_local! {
    /// The worktree root of the task currently executing, or `None` when no task is in scope.
    ///
    /// The executor (issue #32) sets this around all of a task's tool-calling work via
    /// `CURRENT_WORKTREE.scope(Some(root), fut)`; built-in tools read it through
    /// [`TaskWorktreeProvider`]. Each `tokio` task carries its own copy, so concurrent tasks never
    /// observe one another's root (Requirement 2.4).
    pub static CURRENT_WORKTREE: Option<PathBuf>;
}

/// The confinement root returned when no task is in scope (Requirement 2.5).
///
/// It is deliberately a path on the retired drive, so [`crate::path::resolve`] rejects **every**
/// candidate against it with [`RejectReason::RetiredDrive`](crate::path::RejectReason::RetiredDrive)
/// — `path::resolve` runs the retired-drive check first and unconditionally. The originating
/// built-in therefore fails closed, no other task's root is ever returned, and every task's
/// worktree state is left unchanged.
///
/// No real worktree is ever placed here, and nothing ever reads from or writes to it: it exists
/// only to be handed to the resolver and rejected (AGENTS.md hard rule 5 — never touch `C:`). The
/// retired drive is configurable, but this sentinel targets the default `C:` so it is rejected
/// even if a candidate's own drive differs; the resolver's retired-drive comparison uses the
/// configured `resources.retired_drive`, and a root on any drive that is not the configured
/// worktree root is still rejected as an escape.
const NO_TASK_SENTINEL: &str = r"C:\nebula\no-task-in-scope";

/// Per-task implementation of [`WorktreeRootProvider`] (Requirement 2).
///
/// One shared instance serves all concurrent tasks: each call reads its own [`CURRENT_WORKTREE`]
/// copy (Requirement 2.4), so there is no shared mutable "current task" to race on. The daemon
/// installs exactly one of these in place of the static `ConfigWorktreeRoot` (Requirement 2.2).
///
/// The type is zero-sized and `Copy`, so cloning it (for example into an `Arc`) is free.
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskWorktreeProvider;

impl TaskWorktreeProvider {
    /// Create a new provider. Zero-sized; holds no state of its own.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl WorktreeRootProvider for TaskWorktreeProvider {
    /// Resolve the executing task's confinement root (Requirements 2.1, 2.3, 2.5).
    ///
    /// `Some(root)` in [`CURRENT_WORKTREE`] → that root (Requirements 2.1, 2.3); `None`, or no
    /// task-local in scope at all → [`NO_TASK_SENTINEL`], which [`crate::path::resolve`] rejects,
    /// failing the originating built-in closed (Requirement 2.5). The read is a task-local lookup
    /// and a `clone`, well under the 50 ms bound (Requirement 2.3).
    fn worktree_root(&self) -> PathBuf {
        CURRENT_WORKTREE
            .try_with(Clone::clone)
            .ok()
            .flatten()
            .unwrap_or_else(|| PathBuf::from(NO_TASK_SENTINEL))
    }
}

#[cfg(test)]
mod tests {
    use super::{CURRENT_WORKTREE, NO_TASK_SENTINEL, TaskWorktreeProvider};
    use crate::builtins::WorktreeRootProvider;
    use crate::path::{self, Resolved};
    use std::path::PathBuf;

    #[tokio::test]
    async fn scoped_root_is_returned_inside_scope() {
        let provider = TaskWorktreeProvider::new();
        let root = PathBuf::from(r"F:\Nebula\worktrees\task-abc");
        let got = CURRENT_WORKTREE
            .scope(Some(root.clone()), async { provider.worktree_root() })
            .await;
        assert_eq!(got, root, "the provider must return the scoped task root");
    }

    #[tokio::test]
    async fn sentinel_is_returned_outside_any_scope() {
        // No `CURRENT_WORKTREE.scope(..)` wrapping this call: the task-local is unset.
        let provider = TaskWorktreeProvider::new();
        assert_eq!(
            provider.worktree_root(),
            PathBuf::from(NO_TASK_SENTINEL),
            "with no task in scope the provider must yield the rejected sentinel",
        );
    }

    #[tokio::test]
    async fn explicit_none_scope_yields_the_sentinel() {
        let provider = TaskWorktreeProvider::new();
        let got = CURRENT_WORKTREE
            .scope(None, async { provider.worktree_root() })
            .await;
        assert_eq!(
            got,
            PathBuf::from(NO_TASK_SENTINEL),
            "an explicit None scope must behave like no task in scope",
        );
    }

    #[tokio::test]
    async fn concurrent_tasks_each_read_their_own_root() {
        let provider = TaskWorktreeProvider::new();
        let root_a = PathBuf::from(r"F:\Nebula\worktrees\task-a");
        let root_b = PathBuf::from(r"F:\Nebula\worktrees\task-b");

        let a = {
            let root_a = root_a.clone();
            tokio::spawn(async move {
                CURRENT_WORKTREE
                    .scope(Some(root_a), async move {
                        // Yield so the two tasks interleave and would observe each other's root
                        // if there were any shared mutable "current task".
                        tokio::task::yield_now().await;
                        provider.worktree_root()
                    })
                    .await
            })
        };
        let b = {
            let root_b = root_b.clone();
            tokio::spawn(async move {
                CURRENT_WORKTREE
                    .scope(Some(root_b), async move {
                        tokio::task::yield_now().await;
                        provider.worktree_root()
                    })
                    .await
            })
        };

        let (got_a, got_b) = (
            a.await.expect("task a joins"),
            b.await.expect("task b joins"),
        );
        assert_eq!(got_a, root_a, "task a must read its own root");
        assert_eq!(got_b, root_b, "task b must read its own root, never a's");
    }

    /// Out of scope, the provider yields [`NO_TASK_SENTINEL`], and handing that root to
    /// [`crate::path::resolve`] fails closed for every candidate — the originating built-in can
    /// never obtain a permitted path, so no other task's root is ever returned (Requirement 2.5).
    ///
    /// The sentinel is a path on the retired drive (`C:`). The resolver canonicalizes its
    /// confinement root up front; because nothing is ever placed at the sentinel (and `C:` is the
    /// retired drive this machine never touches), that canonicalization cannot succeed, so
    /// `resolve` returns an error rather than ever reaching a `Permitted` outcome. Either way the
    /// result is a fail-closed rejection: it is never `Ok(Resolved::Permitted(_))`. This test only
    /// hands the sentinel to the resolver; it never opens, reads, or writes it (AGENTS.md hard
    /// rule 5 — never touch `C:`).
    #[tokio::test]
    async fn sentinel_out_of_scope_resolves_closed_for_every_candidate() {
        let provider = TaskWorktreeProvider::new();

        // No `CURRENT_WORKTREE.scope(..)`: out of scope, so this is the sentinel.
        let root = provider.worktree_root();
        assert_eq!(
            root,
            PathBuf::from(NO_TASK_SENTINEL),
            "out of scope the provider must return the sentinel",
        );

        // A spread of candidate shapes a built-in might pass: a plain relative name, a nested
        // relative path, an absolute path on another drive, and an absolute path that looks like a
        // real worktree root. None may ever resolve to a permitted path against the sentinel.
        let candidates = [
            PathBuf::from("file.txt"),
            PathBuf::from(r"sub\nested\thing.rs"),
            PathBuf::from(r"F:\Nebula\worktrees\task-other\secret.txt"),
            PathBuf::from(r"D:\data\notes.md"),
        ];

        for candidate in candidates {
            // `retired_drive` is the default `C:`, matching the sentinel's own drive.
            let result = path::resolve(&candidate, &root, "C:");
            match result {
                Ok(Resolved::Permitted(permitted)) => panic!(
                    "sentinel root must never permit a candidate, but {candidate:?} resolved to \
                     Permitted({permitted:?}); a built-in with no task in scope could escape",
                ),
                // Fail-closed: either an explicit rejection or a resolver error (the sentinel's
                // retired-drive root cannot be canonicalized). Both deny the built-in a path.
                Ok(Resolved::Rejected(_)) | Err(_) => {}
            }
        }
    }
}

// Feature: git-worktree-per-task, Property 4: no task in scope yields a root the Path_Resolver rejects
//
// Property 4 (design.md): *For any* candidate path, when the per-task provider is consulted with no
// task in scope (the task-local is `None` or unset), the root it returns causes
// `path::resolve(candidate, root, retired_drive)` to fail closed — the originating built-in never
// receives a `Permitted` path, so no other task's `Worktree_Root` is ever returned and the built-in
// fails (Requirement 2.5).
//
// The provider's no-scope root is `NO_TASK_SENTINEL` (`C:\nebula\no-task-in-scope`), which by design
// targets the retired drive so `path::resolve` rejects it. `path::resolve` canonicalizes the worktree
// root up front, so for this never-created `C:` sentinel the call fails closed in one of two ways, for
// *every* candidate: `Ok(Resolved::Rejected(..))` when the sentinel is canonicalizable, or an `Err`
// (the root cannot be canonicalized) otherwise. Neither outcome is `Permitted`, which is the guarantee
// Requirement 2.5 depends on. The retired drive is passed as the default `"C:"` the sentinel is built
// against. The test only hands the sentinel to `resolve`; it never reads or writes it (AGENTS.md hard
// rule 5 — never touch `C:`).
//
// Gated to Windows because the property reasons about Windows drive-letter (`C:`) path semantics,
// matching the `#[cfg(all(test, windows))]` gating of the resolver's own property tests in `path.rs`.
#[cfg(all(test, windows))]
mod property_no_task_in_scope_is_rejected {
    use super::TaskWorktreeProvider;
    use crate::builtins::WorktreeRootProvider;
    use crate::path::{self, Resolved};
    use proptest::prelude::*;
    use std::path::PathBuf;

    /// Arbitrary candidate paths: a mix of relative and absolute shapes, including `..` escapes and
    /// absolute paths on assorted drive letters, so the property is exercised across a wide candidate
    /// space. Segments stay within safe ASCII so they form valid path components.
    fn candidate_path() -> impl Strategy<Value = PathBuf> {
        let segment = "[a-zA-Z0-9_.-]{1,10}";
        let segments = prop::collection::vec(
            prop_oneof![
                4 => segment.prop_map(String::from),
                1 => Just("..".to_string()),
                1 => Just(".".to_string()),
            ],
            1..6,
        );
        prop_oneof![
            // Relative candidate (joined onto the sentinel root inside `resolve`).
            segments
                .clone()
                .prop_map(|parts| parts.iter().collect::<PathBuf>()),
            // Absolute candidate on some drive letter (taken as-is inside `resolve`).
            ("[A-Za-z]", segments).prop_map(|(drive, parts)| {
                let mut p = PathBuf::from(format!(r"{drive}:\"));
                for part in &parts {
                    p.push(part);
                }
                p
            }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn no_task_in_scope_never_permits_any_candidate(candidate in candidate_path()) {
            // With no `CURRENT_WORKTREE.scope(..)` active the task-local is unset, so the provider
            // yields the sentinel synchronously — no async scope is needed here.
            let provider = TaskWorktreeProvider::new();
            let sentinel_root = provider.worktree_root();

            // The retired drive is the default `"C:"`, the drive the sentinel is built against.
            let result = path::resolve(&candidate, &sentinel_root, "C:");

            // Requirement 2.5's guarantee: the built-in fails closed. The resolver must never return
            // a `Permitted` path for the no-task-in-scope sentinel, so no other task's root is ever
            // handed back. The outcome is either an explicit `Rejected` or a fail-closed `Err` (the
            // `C:` sentinel is never created, so its root cannot be canonicalized) — never `Permitted`.
            prop_assert!(
                !matches!(result, Ok(Resolved::Permitted(_))),
                "no task in scope must never yield a Permitted path (candidate: {candidate:?}, got: {result:?})",
            );
        }
    }
}
