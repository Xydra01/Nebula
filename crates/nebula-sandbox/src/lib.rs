//! The Nebula permission-tier engine and command classifier.
//!
//! This crate owns the data-driven policy that assigns every command a permission
//! [`tier`] ([`Read`], [`Sandbox`], [`Workspace`], [`System`]) and, above the no-approval
//! threshold, the [`approval`] grant that must authorize it. It replaces the deterministic
//! placeholder classifier from issue #27 with a [`rules`]-table-driven [`engine`].
//!
//! # Module tour
//!
//! - [`tier`] — the four-level [`Tier`] vocabulary (`Read`, `Sandbox`, `Workspace`, `System`),
//!   the [`NO_APPROVAL_THRESHOLD`], and [`effective_tier`], the advisory-tier merge that may
//!   raise but never lower the enforced tier.
//! - [`approval`] — the single-use, scope-matched [`Approval`] grant contract: an unforgeable
//!   grant that authorizes exactly one command whose identity, tier, and trace/task id match
//!   its scope, the `ApprovalStore` that consumes it once, the secret-free `ApprovalRequest`
//!   emitted when an above-threshold command lacks a grant, and the `ApprovalRefusal` reasons.
//! - [`classifier`] — the object-safe [`CommandClassifier`] trait (`Arc<dyn CommandClassifier>`)
//!   that `shell.run` consults before starting any child process.
//! - [`rules`] — the serde model for the embedded `rules.toml`, the validating loader
//!   (`RulesTable::load`: rejects a bad tier, a duplicate id, or a malformed rule, naming the
//!   offender), and the command index used for fast matching.
//! - [`engine`] — the [`RulesClassifier`](engine::RulesClassifier): it segments a command line,
//!   forces encoded/indirection sinks to [`Tier::System`], matches each segment against the
//!   rules table (highest-tier-wins, deterministic tie-break, unknown → `System`), resolves path
//!   conditions through a `PathResolver` seam, takes the maximum tier over all segments, gates
//!   anything above the threshold on a scope-matching single-use approval, and emits exactly one
//!   `permit.decision` tracing event per classification.
//! - [`redteam`] — the ≥50-entry `red_team.toml` fixture (every required adversarial category)
//!   that pins the expected tier of known-dangerous commands for the Red_Team_Test.
//! - [`worktree`] — the per-task git worktree lifecycle (issue #29): the `WorktreeManager` the
//!   executor drives, the per-task `WorktreeRootProvider`, and crash recovery. It **reuses** this
//!   crate's [`Approval`] / [`ApprovalRequest`](approval::ApprovalRequest) /
//!   [`ApprovalStore`](approval::ApprovalStore) contract to gate over-limit cleanup deletions,
//!   adding no second approval mechanism.
//!
//! # Protected set
//!
//! This is a protected-set, security-sensitive crate (AGENTS.md hard rule 9): its policy
//! decides what the agent is allowed to run on a real personal machine, so every change is
//! human-reviewed and kept small. The engine performs no filesystem I/O of its own — it
//! delegates all path resolution to its [`PathResolver`](engine::PathResolver) seam — and never
//! reads or writes the retired drive.
//!
//! # Dependency direction
//!
//! `nebula-tools` depends on this crate and re-exports its permission vocabulary; this crate
//! must **not** depend on `nebula-tools`, to avoid a cycle. Because the real FS-accurate path
//! resolver lives in `nebula-tools`, the [`engine`] resolves paths through its own
//! [`PathResolver`](engine::PathResolver) trait: [`RulesClassifier::embedded`](engine::RulesClassifier::embedded)
//! wires a pure-lexical built-in resolver (no filesystem I/O), and a caller may inject an
//! FS-accurate resolver via [`RulesClassifier::with_resolver`](engine::RulesClassifier::with_resolver).
//!
//! [`Read`]: Tier::Read
//! [`Sandbox`]: Tier::Sandbox
//! [`Workspace`]: Tier::Workspace
//! [`System`]: Tier::System
//! [`RulesClassifier`]: engine::RulesClassifier

pub mod approval;
pub mod classifier;
pub mod engine;
#[cfg(windows)]
pub mod job;
pub mod redteam;
pub mod rules;
#[cfg(windows)]
pub mod task_job;
pub mod tier;
pub mod worktree;

// Crate-root re-exports of the permission vocabulary. `nebula-tools` re-exports exactly these
// names from its `permit` module so every issue #27 import (`nebula_tools::permit::Tier` and
// friends) keeps resolving after the relocation (Requirement 8).
pub use approval::Approval;
pub use classifier::CommandClassifier;
pub use tier::{NO_APPROVAL_THRESHOLD, Tier, effective_tier};
