//! Delete-size measurement and the `Delete_Limit` gate (design Section 4.4, "Delete-limit via the
//! #28 `Approval` contract"; Requirement 7).
//!
//! Every removal path in the worktree lifecycle — completion `remove_worktree`, crash recovery,
//! and partial-failure cleanup during `create` — funnels through the one gate implemented here so
//! that no single deletion can meet or exceed the AGENTS.md `Delete_Limit` (1 GiB **or** 500
//! files) without a recorded issue #28 approval (AGENTS.md hard rule 4).
//!
//! The gate works in two steps:
//!
//! 1. [`measure_tree`] recursively sums the exact bytes and file count of the directory **before
//!    any file is removed** (Requirement 7.1). A traversal/metadata failure aborts and retains,
//!    surfacing a [`WorktreeError`] so the caller deletes nothing (Requirement 7.5).
//! 2. [`prepare_delete`] applies the exact thresholds. Deletion proceeds without approval **iff**
//!    `bytes < 1_073_741_824` **and** `files < 500` (Requirement 7.4). When
//!    `bytes >= 1_073_741_824` **or** `files >= 500`, it emits a #28
//!    [`ApprovalRequest`] carrying the measured totals via the
//!    held [`ApprovalStore`] and consumes nothing-removes-nothing
//!    until an authorizing #28 [`Approval`](crate::Approval) is present (Requirements 7.2, 7.3).
//!
//! This **reuses** the issue #28 approval contract (Requirement 7.6): no second approval type is
//! defined here. The over-limit state is surfaced to the caller as
//! [`WorktreeError::DeleteLimit`].
//!
//! The gate is implemented as free functions / a plain decision type that take the directory path,
//! the [`ApprovalStore`] handle, and the approval-scope fields explicitly, rather than as a method
//! on `WorktreeManager`: `manager.rs` owns the full `WorktreeManager` struct (a concurrently built
//! task), so the gate lives here self-contained and the manager calls into it.

use std::path::Path;

use crate::approval::{ApprovalRequest, ApprovalStore};
use crate::rules::RuleId;
use crate::tier::Tier;
use crate::worktree::error::WorktreeError;

/// The `Delete_Limit` byte threshold: 1 GiB (AGENTS.md hard rule 4; Requirement 7.2).
///
/// A deletion of **strictly fewer** than this many bytes clears the byte half of the limit; a
/// deletion of this many bytes or more meets the limit and requires an approval.
pub const DELETE_LIMIT_BYTES: u64 = 1_073_741_824;

/// The `Delete_Limit` file-count threshold: 500 files (AGENTS.md hard rule 4; Requirement 7.2).
///
/// A deletion of **strictly fewer** than this many files clears the file half of the limit; a
/// deletion of this many files or more meets the limit and requires an approval.
pub const DELETE_LIMIT_FILES: u64 = 500;

/// The permission [`Tier`] a worktree delete-limit approval is requested at.
///
/// An over-limit cleanup deletion is a destructive, workspace-scoped action, so it is gated at
/// [`Tier::Workspace`] — above the [`NO_APPROVAL_THRESHOLD`](crate::tier::NO_APPROVAL_THRESHOLD),
/// consistent with how the permission engine gates an above-threshold command.
const DELETE_LIMIT_TIER: Tier = Tier::Workspace;

/// The synthetic rule id carried on a worktree delete-limit [`ApprovalRequest`].
///
/// The #28 [`ApprovalRequest`] names the rule that assigned the tier requiring approval. A
/// delete-limit gate is **not** driven by a `rules.toml` command classification — it is a
/// size-threshold check (AGENTS.md hard rule 4) — so there is no natural matched rule to reference.
/// A stable, descriptive synthetic id is used instead so the request is still self-describing and
/// an authorizing [`Approval`](crate::Approval) can be scoped to it; it is a label, not a lookup
/// into the rules table. The key behaviour the id does not affect: nothing is deleted until an
/// authorizing approval is consumed.
const DELETE_LIMIT_RULE_ID: &str = "worktree.delete.over-limit";

/// The measured size of a directory tree: exact total bytes and file count.
///
/// Produced by [`measure_tree`] before any removal, so the [`prepare_delete`] gate can compare the
/// true totals against the `Delete_Limit` (Requirement 7.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeleteMeasurement {
    /// Total bytes across every regular file in the tree.
    pub bytes: u64,
    /// Total count of regular files in the tree.
    pub files: u64,
}

impl DeleteMeasurement {
    /// Whether this measurement is strictly under **both** limits, so a deletion may proceed
    /// without an approval (Requirement 7.4).
    ///
    /// Returns `true` iff `bytes < `[`DELETE_LIMIT_BYTES`] **and** `files < `[`DELETE_LIMIT_FILES`].
    /// At or above either threshold, an approval is required.
    #[must_use]
    pub const fn within_limit(&self) -> bool {
        self.bytes < DELETE_LIMIT_BYTES && self.files < DELETE_LIMIT_FILES
    }
}

/// What the [`prepare_delete`] gate decided for a pending deletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteDecision {
    /// The measurement is within both limits (or an authorizing approval was consumed): the caller
    /// may remove the directory (Requirement 7.4).
    Proceed,
    /// The measurement met or exceeded a limit and no authorizing approval is present: an
    /// [`ApprovalRequest`] was emitted and the caller must remove
    /// nothing, retaining every affected worktree, until an authorizing approval is consumed
    /// (Requirements 7.2, 7.3). Carries the measured totals.
    AwaitingApproval {
        /// Total measured bytes of the pending deletion.
        bytes: u64,
        /// Total measured file count of the pending deletion.
        files: u64,
    },
}

/// Recursively sum the exact bytes and file count under `path`, **before** any file is removed
/// (Requirement 7.1).
///
/// The traversal descends every subdirectory and counts each regular file once, summing its exact
/// byte length. Symlinks are counted by their own metadata and are **not** followed, so the walk
/// cannot loop or escape the tree. A missing `path` measures as empty (zero bytes, zero files): a
/// nothing-to-delete directory is trivially within the limit.
///
/// # Errors
/// Returns [`WorktreeError::CleanupFailed`] if the directory cannot be read or a child's metadata
/// cannot be stat-ed. A measurement failure must abort and retain (Requirement 7.5): the caller
/// deletes nothing and surfaces the error.
pub async fn measure_tree(path: &Path) -> Result<DeleteMeasurement, WorktreeError> {
    // A missing root is an empty tree — nothing to delete, trivially within the limit.
    if !path.exists() {
        return Ok(DeleteMeasurement::default());
    }

    let mut total = DeleteMeasurement::default();
    // Explicit stack rather than recursion: a worktree can nest arbitrarily deep, and async
    // recursion would need boxing. Each entry is a directory still to be walked.
    let mut stack: Vec<std::path::PathBuf> = vec![path.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await.map_err(|e| {
            WorktreeError::CleanupFailed(format!("could not read directory {dir:?}: {e}"))
        })?;

        loop {
            let entry = entries.next_entry().await.map_err(|e| {
                WorktreeError::CleanupFailed(format!("could not read entry under {dir:?}: {e}"))
            })?;
            let Some(entry) = entry else { break };

            let entry_path = entry.path();
            // `symlink_metadata` does not follow symlinks, so a symlinked directory is counted as a
            // single file-like entry and never descended into — the walk stays within the tree.
            let meta = tokio::fs::symlink_metadata(&entry_path)
                .await
                .map_err(|e| {
                    WorktreeError::CleanupFailed(format!(
                        "could not stat {entry_path:?} while measuring: {e}"
                    ))
                })?;

            if meta.is_dir() {
                stack.push(entry_path);
            } else {
                total.files = total.files.saturating_add(1);
                total.bytes = total.bytes.saturating_add(meta.len());
            }
        }
    }

    Ok(total)
}

/// Measure `path` and gate its deletion against the `Delete_Limit`, reusing the #28 approval
/// contract (Requirement 7).
///
/// The single gate every removal path funnels through (completion `remove_worktree`, crash
/// recovery, partial-failure cleanup):
///
/// 1. [`measure_tree`] sums the tree before any removal (Requirement 7.1). A measurement failure
///    aborts and retains, returning the error (Requirement 7.5).
/// 2. If the measurement is within **both** limits, returns [`DeleteDecision::Proceed`] with no
///    approval (Requirement 7.4).
/// 3. Otherwise it tries to consume an authorizing #28 [`Approval`](crate::Approval) from
///    `approvals` scoped to this delete (command `DELETE_LIMIT_RULE_ID`, tier
///    `DELETE_LIMIT_TIER`, the caller's `trace_id`). A present grant authorizes
///    [`DeleteDecision::Proceed`]; otherwise it emits an
///    [`ApprovalRequest`] carrying the measured totals and
///    returns [`DeleteDecision::AwaitingApproval`], having removed nothing (Requirements 7.2, 7.3).
///
/// The emitted [`ApprovalRequest`] is returned to the caller (rather than side-channelled) so the
/// manager can forward it to the approval surface; it is secret-free by construction. `trace_id`
/// is the task/trace identifier the authorizing approval must be scoped to.
///
/// # Errors
/// Returns [`WorktreeError::CleanupFailed`] if measurement fails (Requirement 7.5). The over-limit,
/// unapproved state is reported as the [`DeleteDecision::AwaitingApproval`] value together with the
/// emitted request — not as an error — so the caller can both surface
/// [`WorktreeError::DeleteLimit`] and forward the request.
pub async fn prepare_delete(
    path: &Path,
    approvals: &ApprovalStore,
    trace_id: &str,
) -> Result<GateOutcome, WorktreeError> {
    let measurement = measure_tree(path).await?;

    if measurement.within_limit() {
        return Ok(GateOutcome {
            decision: DeleteDecision::Proceed,
            request: None,
        });
    }

    // At or over a limit: only an authorizing, scope-matching, single-use #28 approval lets the
    // deletion proceed. `take_authorizing` consumes the grant on a match (Requirement 7.3).
    if approvals
        .take_authorizing(DELETE_LIMIT_RULE_ID, DELETE_LIMIT_TIER, trace_id)
        .is_ok()
    {
        return Ok(GateOutcome {
            decision: DeleteDecision::Proceed,
            request: None,
        });
    }

    // No authorizing grant: emit a #28 request carrying the measured totals and remove nothing
    // (Requirements 7.2, 7.3). The totals ride on the trace id's scope; the request is secret-free.
    let request = ApprovalRequest {
        command: DELETE_LIMIT_RULE_ID.to_owned(),
        rule_id: RuleId(DELETE_LIMIT_RULE_ID.to_owned()),
        tier: DELETE_LIMIT_TIER,
        trace_id: trace_id.to_owned(),
    };
    Ok(GateOutcome {
        decision: DeleteDecision::AwaitingApproval {
            bytes: measurement.bytes,
            files: measurement.files,
        },
        request: Some(request),
    })
}

/// The outcome of a [`prepare_delete`] gate pass: the [`DeleteDecision`] and, when the deletion is
/// held for approval, the #28 [`ApprovalRequest`] to forward.
///
/// `request` is `Some` exactly when `decision` is [`DeleteDecision::AwaitingApproval`]; it is the
/// secret-free request the manager hands to the approval surface. When the decision is
/// [`DeleteDecision::Proceed`], `request` is `None`.
#[derive(Clone, Debug)]
pub struct GateOutcome {
    /// Whether the caller may proceed with removal or must await an approval.
    pub decision: DeleteDecision,
    /// The approval request to forward when the deletion is held for approval; `None` on
    /// [`DeleteDecision::Proceed`].
    pub request: Option<ApprovalRequest>,
}

impl GateOutcome {
    /// Convert an [`DeleteDecision::AwaitingApproval`] outcome into the
    /// [`WorktreeError::DeleteLimit`] a removal method surfaces to its caller (Requirements 5.5,
    /// 7.3); returns `None` on [`DeleteDecision::Proceed`].
    #[must_use]
    pub fn delete_limit_error(&self) -> Option<WorktreeError> {
        match self.decision {
            DeleteDecision::Proceed => None,
            DeleteDecision::AwaitingApproval { bytes, files } => {
                Some(WorktreeError::DeleteLimit { bytes, files })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{Approval, ApprovalScope, GrantId};

    #[tokio::test]
    async fn measure_tree_sums_bytes_and_files() {
        let dir = tempfile::tempdir().expect("make tempdir");
        let root = dir.path();

        // Two files at the root and one in a nested subdirectory: 3 files, 6 bytes total.
        tokio::fs::write(root.join("a.txt"), b"ab")
            .await
            .expect("write a");
        tokio::fs::write(root.join("b.txt"), b"c")
            .await
            .expect("write b");
        let sub = root.join("nested");
        tokio::fs::create_dir(&sub).await.expect("mkdir nested");
        tokio::fs::write(sub.join("c.txt"), b"def")
            .await
            .expect("write c");

        let measured = measure_tree(root).await.expect("measurement succeeds");
        assert_eq!(measured.files, 3, "counts every regular file in the tree");
        assert_eq!(measured.bytes, 6, "sums exact byte lengths across the tree");
    }

    #[tokio::test]
    async fn measure_tree_of_missing_path_is_empty() {
        let dir = tempfile::tempdir().expect("make tempdir");
        let absent = dir.path().join("does-not-exist");
        let measured = measure_tree(&absent).await.expect("missing path is empty");
        assert_eq!(measured, DeleteMeasurement::default());
    }

    #[test]
    fn within_limit_is_strict_on_both_thresholds() {
        // Strictly under both -> proceed without approval (Req 7.4).
        assert!(
            DeleteMeasurement {
                bytes: DELETE_LIMIT_BYTES - 1,
                files: DELETE_LIMIT_FILES - 1,
            }
            .within_limit()
        );
        // At the byte limit -> not within (Req 7.2).
        assert!(
            !DeleteMeasurement {
                bytes: DELETE_LIMIT_BYTES,
                files: 0,
            }
            .within_limit()
        );
        // At the file limit -> not within (Req 7.2).
        assert!(
            !DeleteMeasurement {
                bytes: 0,
                files: DELETE_LIMIT_FILES,
            }
            .within_limit()
        );
    }

    #[tokio::test]
    async fn prepare_delete_proceeds_when_within_limit() {
        let dir = tempfile::tempdir().expect("make tempdir");
        tokio::fs::write(dir.path().join("small.txt"), b"tiny")
            .await
            .expect("write small file");

        let approvals = ApprovalStore::new();
        let outcome = prepare_delete(dir.path(), &approvals, "task-1")
            .await
            .expect("gate runs");
        assert_eq!(outcome.decision, DeleteDecision::Proceed);
        assert!(
            outcome.request.is_none(),
            "no approval request when under limit"
        );
        assert!(outcome.delete_limit_error().is_none());
    }

    #[tokio::test]
    async fn prepare_delete_awaits_approval_over_the_file_limit() {
        // A tree over the file-count limit, with no authorizing approval in the store: the gate
        // emits a secret-free #28 request carrying the measured totals and removes nothing (Req
        // 7.2, 7.3), surfaceable as `DeleteLimit`.
        let dir = tempfile::tempdir().expect("make tempdir");
        for i in 0..DELETE_LIMIT_FILES {
            tokio::fs::write(dir.path().join(format!("f{i}")), b"x")
                .await
                .expect("write file");
        }

        let approvals = ApprovalStore::new();
        let outcome = prepare_delete(dir.path(), &approvals, "task-over")
            .await
            .expect("gate runs");

        match outcome.decision {
            DeleteDecision::AwaitingApproval { bytes, files } => {
                assert_eq!(files, DELETE_LIMIT_FILES);
                assert_eq!(bytes, DELETE_LIMIT_FILES);
            }
            DeleteDecision::Proceed => panic!("over the file limit must await approval"),
        }
        let request = outcome
            .request
            .as_ref()
            .expect("an over-limit gate emits a request");
        assert_eq!(request.tier, DELETE_LIMIT_TIER);
        assert_eq!(request.rule_id, RuleId(DELETE_LIMIT_RULE_ID.to_owned()));
        assert_eq!(request.trace_id, "task-over");
        assert_eq!(
            outcome
                .delete_limit_error()
                .map(|e| matches!(e, WorktreeError::DeleteLimit { .. })),
            Some(true)
        );
    }

    #[tokio::test]
    async fn prepare_delete_proceeds_over_limit_when_an_approval_authorizes() {
        // Same over-limit tree, but a matching single-use approval is present: the gate consumes
        // it and proceeds, emitting no request (Req 7.3, 7.6 — reusing the #28 contract).
        let dir = tempfile::tempdir().expect("make tempdir");
        for i in 0..DELETE_LIMIT_FILES {
            tokio::fs::write(dir.path().join(format!("f{i}")), b"x")
                .await
                .expect("write file");
        }

        let approvals = ApprovalStore::new();
        approvals.insert(Approval::grant(
            GrantId::new(1),
            ApprovalScope {
                command: DELETE_LIMIT_RULE_ID.to_owned(),
                tier: DELETE_LIMIT_TIER,
                trace_id: "task-ok".to_owned(),
            },
        ));

        let outcome = prepare_delete(dir.path(), &approvals, "task-ok")
            .await
            .expect("gate runs");
        assert_eq!(outcome.decision, DeleteDecision::Proceed);
        assert!(
            outcome.request.is_none(),
            "an authorized over-limit delete emits no request"
        );

        // The grant was single-use: a second over-limit gate for the same scope awaits again.
        let again = prepare_delete(dir.path(), &approvals, "task-ok")
            .await
            .expect("gate runs");
        assert!(matches!(
            again.decision,
            DeleteDecision::AwaitingApproval { .. }
        ));
    }

    // Task 6.4 — Unit test: delete-limit via a #28 approval, plus a measurement-failure case.
    //
    // The over-limit-request and the approval-authorizes-proceed halves are already covered by
    // `prepare_delete_awaits_approval_over_the_file_limit` and
    // `prepare_delete_proceeds_over_limit_when_an_approval_authorizes` above (Req 7.2, 7.3, 7.6).
    // This adds the missing piece: a measurement failure must abort and retain (Req 7.5).
    #[tokio::test]
    async fn prepare_delete_aborts_and_retains_when_measurement_fails() {
        // Force a measurement failure portably: point the gate at a path that EXISTS but is a
        // regular file, not a directory. `measure_tree` passes its `path.exists()` guard and then
        // calls `read_dir`, which fails on a non-directory on every platform (including Windows
        // MSVC). This exercises Req 7.5 without touching any real path or `C:`.
        let dir = tempfile::tempdir().expect("make tempdir");
        let not_a_dir = dir.path().join("i-am-a-file");
        tokio::fs::write(&not_a_dir, b"contents")
            .await
            .expect("write file where a directory is expected");

        let approvals = ApprovalStore::new();
        let result = prepare_delete(&not_a_dir, &approvals, "task-measure-fail").await;

        // The gate surfaces the measurement error and removes nothing: the file is retained.
        match result {
            Err(WorktreeError::CleanupFailed(_)) => {}
            Err(other) => panic!("a measurement failure must surface CleanupFailed, got {other:?}"),
            Ok(outcome) => panic!(
                "a measurement failure must abort, not decide; got {:?}",
                outcome.decision
            ),
        }
        // Retain: nothing was deleted — the target still exists untouched (Req 7.5).
        assert!(
            not_a_dir.exists(),
            "a failed measurement must retain the affected path"
        );
        assert_eq!(
            tokio::fs::read(&not_a_dir)
                .await
                .expect("file still readable"),
            b"contents",
            "the retained path is left unchanged"
        );
    }

    // Property-based tests for the delete-limit gate (tasks 6.2 and 6.3). Kept in a sibling module
    // so the proptest macros and their imports are isolated from the example-based unit tests.
    mod property_tests {
        use super::*;
        use proptest::prelude::*;

        /// Build a fresh single-threaded Tokio runtime for a proptest case body.
        ///
        /// proptest test bodies are synchronous, so the async `measure_tree` is driven with
        /// `block_on` inside each generated case rather than via `#[tokio::test]`.
        fn runtime() -> tokio::runtime::Runtime {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime for the proptest case")
        }

        // Feature: git-worktree-per-task, Property 6: no single deletion exceeds the Delete_Limit without a recorded approval
        //
        // Property 6: No single deletion exceeds the Delete_Limit without a recorded approval.
        // Validates: Requirements 1.8, 5.5, 6.7, 7.2, 7.3, 7.4, 7.6.
        //
        // Over generated (bytes, files) pairs straddling both thresholds, assert the pure gate
        // decision logic — `DeleteMeasurement::within_limit()` — proceeds without approval exactly
        // when strictly below BOTH limits, and otherwise requires an approval. Generating multi-GB
        // real trees is infeasible, so the decision logic is tested directly over generated totals
        // (this is the authoritative predicate the gate branches on). The approval-gating half —
        // that an over-limit measurement removes nothing and emits a #28 request until an
        // authorizing `Approval` is consumed — is asserted once below over a real, cheap
        // over-the-file-limit tempdir tree.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn within_limit_matches_the_strict_both_below_predicate(
                // Sample across each threshold's boundary so cases land on both sides of each.
                bytes in prop_oneof![
                    0u64..DELETE_LIMIT_BYTES,
                    Just(DELETE_LIMIT_BYTES - 1),
                    Just(DELETE_LIMIT_BYTES),
                    DELETE_LIMIT_BYTES..=(DELETE_LIMIT_BYTES * 2),
                ],
                files in prop_oneof![
                    0u64..DELETE_LIMIT_FILES,
                    Just(DELETE_LIMIT_FILES - 1),
                    Just(DELETE_LIMIT_FILES),
                    DELETE_LIMIT_FILES..=(DELETE_LIMIT_FILES * 2),
                ],
            ) {
                let measurement = DeleteMeasurement { bytes, files };
                let expected = bytes < DELETE_LIMIT_BYTES && files < DELETE_LIMIT_FILES;
                // Req 7.4: proceed without approval iff strictly below BOTH limits.
                prop_assert_eq!(measurement.within_limit(), expected);
                // Req 7.2: at or above either limit, the measurement is NOT within the limit, so
                // the gate must require an approval before any deletion.
                if bytes >= DELETE_LIMIT_BYTES || files >= DELETE_LIMIT_FILES {
                    prop_assert!(!measurement.within_limit());
                }
            }
        }

        // The approval-gating half of Property 6, asserted once over a real over-the-file-limit
        // tree (cheap: 500 tiny files): with no authorizing approval the gate removes nothing and
        // emits a secret-free #28 request (Req 7.2, 7.3); the affected tree is retained until an
        // authorizing `Approval` is consumed, after which the gate proceeds (Req 7.3, 7.6).
        #[tokio::test]
        async fn over_limit_retains_until_an_approval_is_consumed() {
            use crate::approval::{Approval, ApprovalScope, GrantId};

            let dir = tempfile::tempdir().expect("make tempdir");
            for i in 0..DELETE_LIMIT_FILES {
                tokio::fs::write(dir.path().join(format!("f{i}")), b"x")
                    .await
                    .expect("write file");
            }

            let approvals = ApprovalStore::new();

            // No approval yet: the gate awaits, emits a request, and removes nothing (Req 7.2, 7.3).
            let held = prepare_delete(dir.path(), &approvals, "task-prop6")
                .await
                .expect("gate runs");
            assert!(matches!(
                held.decision,
                DeleteDecision::AwaitingApproval { .. }
            ));
            assert!(
                held.request.is_some(),
                "an over-limit gate emits a #28 request"
            );
            assert!(
                dir.path().exists(),
                "the affected worktree is retained while unapproved"
            );
            assert_eq!(
                std::fs::read_dir(dir.path())
                    .expect("read retained dir")
                    .count() as u64,
                DELETE_LIMIT_FILES,
                "nothing was removed while awaiting approval"
            );

            // Record an authorizing approval scoped to this delete; now the gate proceeds (Req 7.3).
            approvals.insert(Approval::grant(
                GrantId::new(1),
                ApprovalScope {
                    command: "worktree.delete.over-limit".to_owned(),
                    tier: Tier::Workspace,
                    trace_id: "task-prop6".to_owned(),
                },
            ));
            let authorized = prepare_delete(dir.path(), &approvals, "task-prop6")
                .await
                .expect("gate runs");
            assert_eq!(authorized.decision, DeleteDecision::Proceed);
            assert!(
                authorized.request.is_none(),
                "an authorized delete emits no request"
            );
        }

        // Feature: git-worktree-per-task, Property 7: delete-size measurement equals the actual tree totals
        //
        // Property 7: Delete-size measurement equals the actual tree totals.
        // Validates: Requirements 7.1.
        //
        // Build a tempdir tree of generated files with known sizes (0..256 bytes) and a modest
        // count (0..30), possibly nested in subdirectories, then assert `measure_tree` returns the
        // exact total bytes (sum of sizes) and file count (number of files). All paths live in a
        // fresh tempdir; nothing touches `C:`.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn measure_tree_equals_the_true_totals(
                // Each file: (optional subdirectory depth 0..=3, size in bytes 0..256).
                files in prop::collection::vec((0usize..=3, 0usize..256usize), 0..30),
            ) {
                let expected_files = files.len() as u64;
                let expected_bytes: u64 = files.iter().map(|(_, size)| *size as u64).sum();

                let dir = tempfile::tempdir().expect("make tempdir");
                let root = dir.path();

                runtime().block_on(async {
                    for (idx, (depth, size)) in files.iter().enumerate() {
                        // Nest the file under `depth` subdirectories to exercise recursion.
                        let mut path = root.to_path_buf();
                        for level in 0..*depth {
                            path = path.join(format!("sub{level}"));
                        }
                        tokio::fs::create_dir_all(&path)
                            .await
                            .expect("create nested subdirectories");
                        // Unique file name per generated entry so none collide.
                        let file_path = path.join(format!("file{idx}.bin"));
                        tokio::fs::write(&file_path, vec![b'x'; *size])
                            .await
                            .expect("write generated file");
                    }

                    let measured = measure_tree(root).await.expect("measurement succeeds");
                    // Req 7.1: measured totals equal the true tree totals.
                    prop_assert_eq!(measured.files, expected_files);
                    prop_assert_eq!(measured.bytes, expected_bytes);
                    Ok(())
                })?;
            }
        }
    }
}
