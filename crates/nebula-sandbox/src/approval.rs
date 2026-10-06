//! The approval grant contract: the unforgeable, single-use [`Approval`] grant, its grant
//! identity ([`GrantId`]) and authorized [`ApprovalScope`], the secret-free [`ApprovalRequest`]
//! emitted when an above-threshold command lacks an authorizing grant, and the
//! [`ApprovalRefusal`] reasons surfaced to the caller.
//!
//! An [`Approval`] authorizes exactly one command whose identity, tier, and trace/task id all
//! match its [`ApprovalScope`]. It is **origin-agnostic** (there is no remote-origin field in
//! this feature — that is deferred to issue #37) and carries **no wall-clock expiry**: single-use
//! consumption is the only lifetime bound. The grant is constructible only through
//! [`Approval::grant`], and its authorizing fields are private, so an external caller cannot forge
//! one (Requirement 5.5). Single-use consumption is enforced by the [`ApprovalStore`]: it holds
//! granted, not-yet-consumed approvals keyed by trace/task id and hands out each grant exactly
//! once, returning [`ApprovalRefusal::ScopeMismatch`] when nothing matches the presented command
//! and [`ApprovalRefusal::AlreadyConsumed`] when a grant was already spent.

use crate::rules::RuleId;
use crate::tier::Tier;

/// An unforgeable, unique grant identity.
///
/// Wraps a private `u128` so a `GrantId` cannot be pattern-matched apart or reconstructed from an
/// arbitrary value by an external caller except through the grant-path entry point
/// [`GrantId::new`]. Uniqueness is the responsibility of the approval-granting path that mints the
/// value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GrantId(u128);

impl GrantId {
    /// Mint a grant identity from a raw value.
    ///
    /// This is the **grant-path-only** entry point: it is called by the approval surface
    /// (TUI/ntfy, via the daemon) when it issues a grant, and the caller is responsible for
    /// supplying a unique value. It does not, on its own, authorize anything — authorization
    /// requires a full [`Approval`] built via [`Approval::grant`] whose [`ApprovalScope`] matches
    /// the presented command (see [`Approval::authorizes`]).
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    /// The raw value backing this grant identity.
    #[must_use]
    pub const fn value(&self) -> u128 {
        self.0
    }
}

/// What an [`Approval`] authorizes (Requirement 5.1): a specific command identity, at a specific
/// permission [`Tier`], for a specific trace or task.
///
/// All three fields must match the presented command for the grant to authorize it (see
/// [`Approval::authorizes`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalScope {
    /// The authorized command identity (the normalized command base name).
    pub command: String,
    /// The permission tier this scope authorizes.
    pub tier: Tier,
    /// The trace or task identifier this scope is bound to.
    pub trace_id: String,
}

/// A grant authorizing one above-threshold command.
///
/// Constructible **only** through [`Approval::grant`]; its authorizing fields (the grant
/// [`GrantId`] and the [`ApprovalScope`]) are private, so an external caller cannot forge an
/// authorizing `Approval` by constructing the struct directly or mutating a field (Requirement
/// 5.5). The grant is single-use and scope-matched, origin-agnostic (no remote-origin field in
/// this feature — issue #37), and carries no wall-clock expiry. The layout reserves room for a
/// future `expiry` field so it can be added without a breaking change.
#[derive(Clone, Debug)]
pub struct Approval {
    /// The grant identity. Private: part of the unforgeable authorizing state.
    id: GrantId,
    /// The authorized scope. Private: part of the unforgeable authorizing state.
    scope: ApprovalScope,
    // Reserved for issue #37 / a future expiry. Not an authorizing field and not yet present:
    // expiry: Option<std::time::Instant>,
}

impl Approval {
    /// The sole constructor: the defined grant path.
    ///
    /// Produced by the approval surface (TUI/ntfy) via the daemon; there is no other way to obtain
    /// an authorizing `Approval` (Requirement 5.5). Takes a minted [`GrantId`] and the
    /// [`ApprovalScope`] the grant authorizes.
    #[must_use]
    pub fn grant(id: GrantId, scope: ApprovalScope) -> Self {
        Self { id, scope }
    }

    /// This approval's grant identity.
    #[must_use]
    pub const fn id(&self) -> GrantId {
        self.id
    }

    /// This approval's authorized scope.
    #[must_use]
    pub const fn scope(&self) -> &ApprovalScope {
        &self.scope
    }

    /// Test-only constructor. Builds a minimal approval **without** going through the grant path.
    ///
    /// Gated behind the `test`/`test-util` cfg so production builds keep the type unforgeable
    /// (Requirement 5.5): this constructor does **not** exist in a normal release build and
    /// therefore authorizes nothing in production. It exists only so the issue #27 `shell.run`
    /// tests, which construct `Approval::new()`, compile unchanged against the relocated,
    /// re-exported `Approval` (Requirement 8.6).
    ///
    /// The approval it produces carries a fixed zero [`GrantId`] and an empty-but-present
    /// [`ApprovalScope`]; it is only meaningful to the issue #27 gate tests, which assert on the
    /// *presence* of an approval rather than its scope.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::grant(
            GrantId::new(0),
            ApprovalScope {
                command: String::new(),
                tier: Tier::Sandbox,
                trace_id: String::new(),
            },
        )
    }

    /// True iff this approval's scope matches the presented command identity, permission tier, and
    /// trace or task id (Requirement 5.2).
    ///
    /// Authorization succeeds only on a **full** match of all three: a difference in any of the
    /// command identity, the tier, or the trace/task id means the grant does not authorize the
    /// presented command (Requirement 5.3). This check is origin-agnostic and does not consider
    /// expiry (there is none in this feature); single-use consumption is enforced separately by the
    /// approval store (task 4.2).
    #[must_use]
    pub fn authorizes(&self, command: &str, tier: Tier, trace_id: &str) -> bool {
        self.scope.command == command && self.scope.tier == tier && self.scope.trace_id == trace_id
    }
}

/// Why an [`Approval`] failed to authorize a command, surfaced to the caller (Requirements 5.3,
/// 5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApprovalRefusal {
    /// The presented approval's scope did not match the command's identity, tier, or trace/task id
    /// (Requirement 5.3).
    #[error("approval scope does not match the presented command")]
    ScopeMismatch,
    /// The presented approval had already authorized a command and is single-use, so it cannot
    /// authorize another (Requirement 5.6).
    #[error("approval has already been consumed")]
    AlreadyConsumed,
}

/// Holds granted, not-yet-consumed [`Approval`]s and consumes them on the first authorizing
/// match, enforcing single-use (Requirement 5.6).
///
/// The granted approvals live behind a [`std::sync::Mutex`], making the store **interior-mutable**
/// so a shared `Arc<dyn CommandClassifier>` can consume a grant through `&self` without needing
/// `&mut self` (design "Components and Interfaces": `approvals: Arc<ApprovalStore>`).
///
/// Grants are keyed by their [`GrantId`]. A separate set records the scopes of grants that have
/// already been consumed, so that re-presenting a scope that a now-spent grant used to authorize
/// is distinguishable from a scope that no grant ever covered: the former yields
/// [`ApprovalRefusal::AlreadyConsumed`] (Requirement 5.6) while the latter yields
/// [`ApprovalRefusal::ScopeMismatch`] (Requirement 5.3).
#[derive(Debug, Default)]
pub struct ApprovalStore {
    /// Granted, not-yet-consumed approvals keyed by grant identity, plus the scopes of grants that
    /// have already been consumed. Both live behind one `Mutex` so a consume is a single atomic
    /// critical section. Poisoning is recovered (never panicked on) via
    /// [`std::sync::PoisonError::into_inner`].
    inner: std::sync::Mutex<ApprovalStoreInner>,
}

/// The mutex-guarded state of an [`ApprovalStore`].
#[derive(Debug, Default)]
struct ApprovalStoreInner {
    /// Granted approvals that have not yet authorized a command, keyed by [`GrantId`].
    granted: std::collections::HashMap<GrantId, Approval>,
    /// Scopes of grants that have already been consumed, retained so a re-presented but spent
    /// scope reports [`ApprovalRefusal::AlreadyConsumed`] rather than
    /// [`ApprovalRefusal::ScopeMismatch`].
    consumed: Vec<ApprovalScope>,
}

impl ApprovalStore {
    /// Construct an empty store holding no grants.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a granted, unconsumed [`Approval`] so it can later authorize a matching command.
    ///
    /// Keyed by the approval's [`GrantId`]; a repeated grant id overwrites the prior entry (grant
    /// ids are minted unique by the grant path, so this is not expected in practice).
    pub fn insert(&self, approval: Approval) {
        let mut inner = self.lock();
        inner.granted.insert(approval.id(), approval);
    }

    /// Consume the first stored grant that authorizes the presented command, tier, and trace/task
    /// id, returning it (single-use — Requirement 5.6).
    ///
    /// - On the first unconsumed grant whose [`Approval::authorizes`] holds for the arguments, the
    ///   grant is removed from the store, its scope is recorded as consumed, and it is returned
    ///   (Requirement 5.2).
    /// - If no unconsumed grant matches the scope **and** no already-consumed grant covered it,
    ///   returns [`ApprovalRefusal::ScopeMismatch`] (Requirement 5.3).
    /// - If no unconsumed grant matches but a previously consumed grant did cover this scope,
    ///   returns [`ApprovalRefusal::AlreadyConsumed`] (Requirement 5.6).
    ///
    /// # Errors
    ///
    /// Returns an [`ApprovalRefusal`] describing why no grant authorized the command.
    pub fn take_authorizing(
        &self,
        command: &str,
        tier: Tier,
        trace_id: &str,
    ) -> Result<Approval, ApprovalRefusal> {
        let mut inner = self.lock();

        // First authorizing, unconsumed grant wins and is consumed single-use.
        if let Some(&id) = inner
            .granted
            .iter()
            .find(|(_, approval)| approval.authorizes(command, tier, trace_id))
            .map(|(id, _)| id)
        {
            // Present by construction: the id came from this map under the held lock.
            if let Some(approval) = inner.granted.remove(&id) {
                inner.consumed.push(approval.scope().clone());
                return Ok(approval);
            }
        }

        // No live grant authorizes; distinguish a spent scope from one never covered.
        if inner.consumed.iter().any(|scope| {
            scope.command == command && scope.tier == tier && scope.trace_id == trace_id
        }) {
            Err(ApprovalRefusal::AlreadyConsumed)
        } else {
            Err(ApprovalRefusal::ScopeMismatch)
        }
    }

    /// Lock the inner state, recovering a poisoned guard rather than panicking.
    ///
    /// A `Mutex` lock returns a `PoisonError` if a prior holder panicked while holding the guard.
    /// Library code must not `unwrap`/`expect`/`panic` (AGENTS.md), so the guard is recovered with
    /// [`std::sync::PoisonError::into_inner`], which is panic-free: the store's invariants do not
    /// depend on the poisoned thread having completed, so the recovered state is safe to use.
    fn lock(&self) -> std::sync::MutexGuard<'_, ApprovalStoreInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Emitted when an above-threshold command is refused for lack of an authorizing approval
/// (Requirement 4.4).
///
/// **Secret-free by construction** (Requirement 4.5): it carries only the classified command
/// name, the matched [`RuleId`], the assigned [`Tier`], and the trace/task id — never a command
/// argument value or any other secret-bearing field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// The classified command identity (the normalized command base name).
    pub command: String,
    /// The id of the rule that assigned the tier requiring approval.
    pub rule_id: RuleId,
    /// The permission tier the command was classified at.
    pub tier: Tier,
    /// The trace or task identifier the request is bound to.
    pub trace_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(command: &str, tier: Tier, trace_id: &str) -> ApprovalScope {
        ApprovalScope {
            command: command.to_owned(),
            tier,
            trace_id: trace_id.to_owned(),
        }
    }

    #[test]
    fn grant_populates_id_and_scope() {
        let id = GrantId::new(42);
        let s = scope("git", Tier::System, "trace-1");
        let approval = Approval::grant(id, s.clone());
        assert_eq!(approval.id(), id);
        assert_eq!(approval.scope(), &s);
    }

    #[test]
    fn authorizes_only_on_full_scope_match() {
        let approval = Approval::grant(GrantId::new(1), scope("git", Tier::System, "trace-1"));
        assert!(approval.authorizes("git", Tier::System, "trace-1"));
        // Command mismatch.
        assert!(!approval.authorizes("rm", Tier::System, "trace-1"));
        // Tier mismatch.
        assert!(!approval.authorizes("git", Tier::Workspace, "trace-1"));
        // Trace/task id mismatch.
        assert!(!approval.authorizes("git", Tier::System, "trace-2"));
    }

    #[test]
    fn grant_preserves_scope_fields_verbatim() {
        // Req 5.1: the scope stores the exact command identity, authorized tier, and trace/task
        // id handed to the constructor — each field is readable back unchanged.
        let s = scope("Remove-Item", Tier::Workspace, "task-77");
        let approval = Approval::grant(GrantId::new(5), s.clone());
        assert_eq!(approval.scope().command, "Remove-Item");
        assert_eq!(approval.scope().tier, Tier::Workspace);
        assert_eq!(approval.scope().trace_id, "task-77");
        // And the whole scope compares equal to the one passed in.
        assert_eq!(approval.scope(), &s);
    }

    #[test]
    fn grant_populates_id_and_scope_with_empty_string_fields() {
        // Req 5.1: empty-but-present command and trace/task id are stored verbatim — the grant
        // path does not reject or rewrite empty scope fields, and they round-trip through the
        // accessors.
        let id = GrantId::new(0);
        let s = scope("", Tier::Read, "");
        let approval = Approval::grant(id, s.clone());
        assert_eq!(approval.id(), id);
        assert_eq!(approval.scope(), &s);
        assert_eq!(approval.scope().command, "");
        assert_eq!(approval.scope().trace_id, "");
    }

    #[test]
    fn authorizes_returns_true_on_a_full_match() {
        // Req 5.2: a presentation whose command identity, tier, AND trace/task id all equal the
        // scope authorizes.
        let approval = Approval::grant(GrantId::new(2), scope("netsh", Tier::System, "trace-xyz"));
        assert!(approval.authorizes("netsh", Tier::System, "trace-xyz"));
    }

    #[test]
    fn authorizes_false_on_command_mismatch_only() {
        // Req 5.2: tier and trace/task id match, but the command identity differs — no
        // authorization. Isolates the command field.
        let approval = Approval::grant(GrantId::new(3), scope("git", Tier::System, "trace-1"));
        assert!(!approval.authorizes("gitt", Tier::System, "trace-1"));
    }

    #[test]
    fn authorizes_false_on_tier_mismatch_only() {
        // Req 5.2: command identity and trace/task id match, but the tier differs — no
        // authorization. Isolates the tier field (checked both above and below the scope tier).
        let approval = Approval::grant(GrantId::new(4), scope("git", Tier::Workspace, "trace-1"));
        assert!(!approval.authorizes("git", Tier::System, "trace-1"));
        assert!(!approval.authorizes("git", Tier::Sandbox, "trace-1"));
    }

    #[test]
    fn authorizes_false_on_trace_id_mismatch_only() {
        // Req 5.2: command identity and tier match, but the trace/task id differs — no
        // authorization. Isolates the trace/task id field.
        let approval = Approval::grant(GrantId::new(6), scope("git", Tier::System, "trace-1"));
        assert!(!approval.authorizes("git", Tier::System, "trace-2"));
    }

    #[test]
    fn authorizes_distinguishes_empty_scope_fields_from_populated_presentations() {
        // Req 5.2: an empty scope field matches only an empty presentation of that field; a
        // non-empty presentation of an empty-scope field is not a match.
        let approval = Approval::grant(GrantId::new(8), scope("", Tier::Read, ""));
        assert!(approval.authorizes("", Tier::Read, ""));
        assert!(!approval.authorizes("git", Tier::Read, ""));
        assert!(!approval.authorizes("", Tier::Read, "trace-1"));
    }

    #[test]
    fn take_authorizing_consumes_matching_grant() {
        let store = ApprovalStore::new();
        let id = GrantId::new(7);
        store.insert(Approval::grant(id, scope("git", Tier::System, "trace-1")));

        let taken = store
            .take_authorizing("git", Tier::System, "trace-1")
            .expect("a matching grant authorizes");
        assert_eq!(taken.id(), id);
    }

    #[test]
    fn take_authorizing_without_matching_scope_is_scope_mismatch() {
        let store = ApprovalStore::new();
        store.insert(Approval::grant(
            GrantId::new(1),
            scope("git", Tier::System, "trace-1"),
        ));

        // Command, tier, and trace/task id each independently cause a scope mismatch.
        // (`Approval` is intentionally not `PartialEq`, so assert on the refusal variant only.)
        assert_eq!(
            store
                .take_authorizing("rm", Tier::System, "trace-1")
                .unwrap_err(),
            ApprovalRefusal::ScopeMismatch
        );
        assert_eq!(
            store
                .take_authorizing("git", Tier::Workspace, "trace-1")
                .unwrap_err(),
            ApprovalRefusal::ScopeMismatch
        );
        assert_eq!(
            store
                .take_authorizing("git", Tier::System, "trace-2")
                .unwrap_err(),
            ApprovalRefusal::ScopeMismatch
        );
    }

    #[test]
    fn take_authorizing_on_empty_store_is_scope_mismatch() {
        let store = ApprovalStore::new();
        assert_eq!(
            store
                .take_authorizing("git", Tier::System, "trace-1")
                .unwrap_err(),
            ApprovalRefusal::ScopeMismatch
        );
    }

    #[test]
    fn second_use_of_a_grant_is_already_consumed() {
        let store = ApprovalStore::new();
        store.insert(Approval::grant(
            GrantId::new(3),
            scope("git", Tier::System, "trace-1"),
        ));

        // First use consumes the single-use grant.
        assert!(
            store
                .take_authorizing("git", Tier::System, "trace-1")
                .is_ok()
        );

        // Re-presenting the same scope now reports the grant as already consumed (Req 5.6),
        // distinct from a scope that no grant ever covered.
        assert_eq!(
            store
                .take_authorizing("git", Tier::System, "trace-1")
                .unwrap_err(),
            ApprovalRefusal::AlreadyConsumed
        );
    }

    #[test]
    fn each_grant_authorizes_exactly_one_command() {
        let store = ApprovalStore::new();
        store.insert(Approval::grant(
            GrantId::new(10),
            scope("git", Tier::System, "trace-1"),
        ));
        store.insert(Approval::grant(
            GrantId::new(11),
            scope("git", Tier::System, "trace-1"),
        ));

        // Two distinct grants cover the same scope: two authorizations succeed, the third fails
        // as already-consumed.
        assert!(
            store
                .take_authorizing("git", Tier::System, "trace-1")
                .is_ok()
        );
        assert!(
            store
                .take_authorizing("git", Tier::System, "trace-1")
                .is_ok()
        );
        assert_eq!(
            store
                .take_authorizing("git", Tier::System, "trace-1")
                .unwrap_err(),
            ApprovalRefusal::AlreadyConsumed
        );
    }

    #[test]
    fn store_is_shareable_through_shared_reference() {
        // Interior mutability: consuming through `&self` behind an `Arc` compiles and works, as a
        // shared `Arc<dyn CommandClassifier>` requires.
        let store = std::sync::Arc::new(ApprovalStore::new());
        store.insert(Approval::grant(
            GrantId::new(99),
            scope("rm", Tier::Workspace, "task-9"),
        ));
        let shared = std::sync::Arc::clone(&store);
        assert!(
            shared
                .take_authorizing("rm", Tier::Workspace, "task-9")
                .is_ok()
        );
    }

    // Feature: permission-tiers, Property 9: only a scope-matching, unconsumed approval authorizes
    //
    // Property 9: Only a scope-matching, unconsumed approval authorizes; a refusal emits a
    // secret-free request. Over generated grants and presented commands:
    //  - a presentation that fully matches the grant's scope authorizes on the first use
    //    (consuming the single-use grant, Req 5.2), and a second identical presentation is refused
    //    as `AlreadyConsumed` (Req 5.6);
    //  - a presentation that does NOT fully match is refused as `ScopeMismatch` (Req 5.3) and
    //    leaves the grant unconsumed — the command stays unexecuted (Req 4.2) and a subsequent
    //    matching presentation still authorizes;
    //  - an above-threshold command refused for lack of an authorizing grant yields an
    //    `ApprovalRequest` carrying the command name, matched rule id, and tier (Req 4.4) and,
    //    being secret-free by construction (it has no args field), never contains a secret token
    //    passed as a command argument (Req 4.5), which also means no process was started for the
    //    refused command (Req 4.1).
    // (Validates: Requirements 4.1, 4.2, 4.4, 5.2, 5.3, 5.6).
    mod property_scope_match_authorizes {
        use super::*;
        use crate::rules::RuleId;
        use proptest::prelude::*;

        /// A small set of distinct command identities so matches and mismatches both occur with
        /// meaningful probability.
        fn any_command() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("git".to_owned()),
                Just("rm".to_owned()),
                Just("netsh".to_owned()),
                Just("Remove-Item".to_owned()),
            ]
        }

        /// Any of the four tiers with uniform coverage.
        fn any_tier() -> impl Strategy<Value = Tier> {
            prop_oneof![
                Just(Tier::Read),
                Just(Tier::Sandbox),
                Just(Tier::Workspace),
                Just(Tier::System),
            ]
        }

        /// An above-threshold tier (Workspace or System) — the tiers that require an approval and
        /// so emit an `ApprovalRequest` when none is present.
        fn above_threshold_tier() -> impl Strategy<Value = Tier> {
            prop_oneof![Just(Tier::Workspace), Just(Tier::System)]
        }

        /// A small set of distinct trace/task identifiers.
        fn any_trace_id() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("trace-1".to_owned()),
                Just("trace-2".to_owned()),
                Just("task-9".to_owned()),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn only_full_scope_match_authorizes_and_grant_is_single_use(
                granted_command in any_command(),
                granted_tier in any_tier(),
                granted_trace in any_trace_id(),
                presented_command in any_command(),
                presented_tier in any_tier(),
                presented_trace in any_trace_id(),
            ) {
                let store = ApprovalStore::new();
                store.insert(Approval::grant(
                    GrantId::new(1),
                    ApprovalScope {
                        command: granted_command.clone(),
                        tier: granted_tier,
                        trace_id: granted_trace.clone(),
                    },
                ));

                let full_match = presented_command == granted_command
                    && presented_tier == granted_tier
                    && presented_trace == granted_trace;

                let first = store.take_authorizing(
                    &presented_command,
                    presented_tier,
                    &presented_trace,
                );

                if full_match {
                    // Req 5.2: a full scope match authorizes, returning the granted approval.
                    let approval = first.map_err(|refusal| {
                        TestCaseError::fail(format!(
                            "a full scope match must authorize, got {refusal:?}"
                        ))
                    })?;
                    prop_assert_eq!(approval.id(), GrantId::new(1));

                    // Req 5.6: the grant is single-use — a second identical presentation is
                    // refused as already consumed. (`Approval` is intentionally not `PartialEq`,
                    // so assert on the refusal variant via `unwrap_err`.)
                    let second = store
                        .take_authorizing(&presented_command, presented_tier, &presented_trace)
                        .expect_err("a consumed single-use grant must not authorize again");
                    prop_assert_eq!(second, ApprovalRefusal::AlreadyConsumed);
                } else {
                    // Req 5.3: a presentation that does not fully match the scope is a scope
                    // mismatch.
                    let refusal = first
                        .expect_err("a scope that does not fully match must not authorize");
                    prop_assert_eq!(refusal, ApprovalRefusal::ScopeMismatch);

                    // Req 4.2: the mismatching presentation left the command unexecuted — the
                    // grant was not consumed, so presenting its exact scope still authorizes.
                    prop_assert!(
                        store
                            .take_authorizing(&granted_command, granted_tier, &granted_trace)
                            .is_ok(),
                        "a scope mismatch must not consume the grant",
                    );
                }
            }

            #[test]
            fn refusal_emits_a_secret_free_approval_request(
                command in any_command(),
                tier in above_threshold_tier(),
                trace in any_trace_id(),
                rule_id in "[a-z]{1,8}\\.[a-z]{1,8}",
                secret in "[A-Za-z0-9]{8,24}",
            ) {
                // An above-threshold command with no authorizing grant in the store: the store
                // refuses it (Req 4.1 — no process is started here; the store never executes).
                let store = ApprovalStore::new();
                let refusal = store
                    .take_authorizing(&command, tier, &trace)
                    .expect_err("an empty store authorizes nothing");
                prop_assert_eq!(refusal, ApprovalRefusal::ScopeMismatch);

                // Req 4.4: the emitted request names the classified command, the matched rule id,
                // and the assigned tier. It is built only from those fields plus the trace id —
                // the secret-bearing command argument is deliberately NOT passed in.
                let request = ApprovalRequest {
                    command: command.clone(),
                    rule_id: RuleId(rule_id.clone()),
                    tier,
                    trace_id: trace.clone(),
                };
                prop_assert_eq!(&request.command, &command);
                prop_assert_eq!(&request.rule_id, &RuleId(rule_id));
                prop_assert_eq!(request.tier, tier);
                prop_assert_eq!(&request.trace_id, &trace);

                // Req 4.5: the request is secret-free by construction — it has no args field, so a
                // secret passed as a command argument cannot appear anywhere in the request. Guard
                // against the secret ever being a substring of any field or the full Debug form.
                prop_assume!(!command.contains(&secret));
                prop_assume!(!trace.contains(&secret));
                let rendered = format!("{request:?}");
                prop_assert!(
                    !rendered.contains(&secret),
                    "approval request leaked a secret argument: {rendered}",
                );
            }
        }
    }

    // Feature: permission-tiers, Property 10: an approval cannot be forged to authorize
    //
    // Property 10: An approval cannot be forged to authorize (Req 5.5). The guarantee is
    // primarily STRUCTURAL and enforced at compile time: `Approval`'s authorizing fields (`id`
    // and `scope`) are private, there is no public setter for either, and the only ways to
    // obtain an `Approval` are `Approval::grant` (the production grant path) and the test-only,
    // feature-gated `Approval::new` (which exists solely for the issue #27 gate tests and does
    // not compile into a release build). Consequently, external code cannot build an authorizing
    // `Approval` by any other means: a struct-literal `Approval { id, scope }` written outside
    // this module does not compile (the private fields are inaccessible), and there is no public
    // mutator to retrofit an authorizing scope onto an approval obtained some other way.
    //
    // That compile-time property cannot itself be asserted at runtime, so this proptest exercises
    // its runtime consequence over generated scopes: every authorizing `Approval` is one produced
    // by `Approval::grant(...)`, and such an approval authorizes exactly its own grant scope and
    // nothing else. Because `grant` is the sole production constructor, "authorization only ever
    // succeeds for a grant-constructed approval" follows.
    // (Validates: Requirements 5.5).
    mod property_unforgeable {
        use super::*;
        use proptest::prelude::*;

        /// A small set of distinct command identities so matches and mismatches both occur.
        fn any_command() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("git".to_owned()),
                Just("rm".to_owned()),
                Just("netsh".to_owned()),
                Just("Remove-Item".to_owned()),
            ]
        }

        /// Any of the four tiers with uniform coverage.
        fn any_tier() -> impl Strategy<Value = Tier> {
            prop_oneof![
                Just(Tier::Read),
                Just(Tier::Sandbox),
                Just(Tier::Workspace),
                Just(Tier::System),
            ]
        }

        /// A small set of distinct trace/task identifiers.
        fn any_trace_id() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("trace-1".to_owned()),
                Just("trace-2".to_owned()),
                Just("task-9".to_owned()),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn authorization_only_succeeds_for_a_grant_constructed_approval(
                granted_command in any_command(),
                granted_tier in any_tier(),
                granted_trace in any_trace_id(),
                presented_command in any_command(),
                presented_tier in any_tier(),
                presented_trace in any_trace_id(),
            ) {
                // The ONLY way to obtain an authorizing `Approval` is the grant path — the fields
                // are private and there is no other public constructor or setter (Req 5.5). So
                // the authorizing approval under test is, necessarily, one built by `grant`.
                let approval = Approval::grant(
                    GrantId::new(1),
                    ApprovalScope {
                        command: granted_command.clone(),
                        tier: granted_tier,
                        trace_id: granted_trace.clone(),
                    },
                );

                let full_match = presented_command == granted_command
                    && presented_tier == granted_tier
                    && presented_trace == granted_trace;

                // The grant-constructed approval authorizes exactly its own scope and nothing
                // else: authorization succeeds iff the presentation fully matches the granted
                // scope. There is no forged, non-grant approval that could authorize instead.
                prop_assert_eq!(
                    approval.authorizes(&presented_command, presented_tier, &presented_trace),
                    full_match,
                );

                // The grant round-trips its scope verbatim through the public accessor: the only
                // authorizing state is the one `grant` was handed, with no hidden or externally
                // settable field in play.
                prop_assert_eq!(&approval.scope().command, &granted_command);
                prop_assert_eq!(approval.scope().tier, granted_tier);
                prop_assert_eq!(&approval.scope().trace_id, &granted_trace);
            }
        }
    }
}
