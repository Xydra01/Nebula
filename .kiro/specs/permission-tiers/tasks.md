# Implementation Plan: Permission Tiers and Command Classifier (nebula-sandbox)

## Overview

This plan builds the real **permission-tier engine and command classifier** for Nebula (GitHub issue #28, Phase 1 exit criterion 2), replacing the deterministic `DefaultClassifier` placeholder from issue #27 with a data-driven engine in a new `nebula-sandbox` crate. It is a protected-set, security-sensitive feature (AGENTS.md hard rule 9), so tasks are strictly incremental and test-driven: the new crate and the vocabulary relocation land first and keep every issue #27 call site green, then the enriched approval contract, then the rules-table loader and the checked-in `rules.toml` data, then the classifier engine (segmentation, encoding detection, matching, unknown→Tier 3, path conditions, segment-max, monotonicity), then decision logging, then the red-team fixture and its test, then the property tests, then daemon wiring and the config key, and finally a checkpoint and a (never-merged) PR.

The relocation is the keystone: `Tier`, `NO_APPROVAL_THRESHOLD`, `Approval`, the `CommandClassifier` trait, and `effective_tier` move from `nebula-tools/src/permit.rs` into `nebula-sandbox`; `nebula-tools` depends on `nebula-sandbox` and re-exports those names so `shell.rs`, `builtin_providers.rs`, the host, and every issue #27 test keep compiling and passing with no edits (Requirement 8).

The design defines 12 correctness properties (P1–P12). Each is implemented as a single `proptest` property test (minimum 100 iterations), placed next to the code it verifies, and tagged `// Feature: permission-tiers, Property N: ...` with a `Validates: Requirements X.Y` annotation. The issue #28 acceptance tests (the ≥50-entry red-team fixture with full category coverage; load validation; approval scope/single-use/unforgeability) are included as explicit tasks.

Language: Rust (used throughout the design and the existing crates). Conventions enforced per task: `thiserror` in libraries, `anyhow` only in binaries, no `unwrap`/`expect`/`panic` in library code (clippy deny; test files carry the allow header), `unsafe` denied (none is needed here), doc comments on every public item and a module doc on each `lib.rs`, serde structs with `#[serde(deny_unknown_fields)]`, any new config key in BOTH `config/default.toml` and `nebula-config`, and all tests redirect paths to tempdirs and never touch real paths or `C:`. Target: Windows MSVC.

## Tasks

- [x] 1. Create the `nebula-sandbox` crate and wire it into the workspace
  - [x] 1.1 Scaffold the crate, workspace membership, dependencies, and module tree
    - Create `crates/nebula-sandbox/Cargo.toml` as a new workspace member; add it to the root `Cargo.toml` `[workspace] members` list
    - Inherit the workspace lints (`[lints] workspace = true`) so no-panic / unsafe-denied rules apply; `unsafe` stays denied (no `unsafe` is needed in this crate)
    - Depend on `thiserror`, `serde` (derive), `toml`, and `tracing` from `[workspace.dependencies]`; add a dependency on the crate that owns `path::resolve` (the `Path_Resolver` seam) ONLY, avoiding a dependency cycle by keeping that crate free of a reverse dependency on `nebula-sandbox`
    - Add `proptest` and `tempfile` to `[dev-dependencies]` referencing the workspace versions
    - Create `crates/nebula-sandbox/src/lib.rs` with a crate module doc describing the permission-tier engine and its protected-set status; declare the empty module tree (`tier`, `approval`, `classifier`, `rules`, `engine`, `redteam`) to be filled by later tasks
    - _Requirements: 1.1_

- [x] 2. Relocate the permission vocabulary into `nebula-sandbox` and re-export from `nebula-tools`
  - [x] 2.1 Move `Tier`, `NO_APPROVAL_THRESHOLD`, and `effective_tier` into `nebula-sandbox`
    - Create `crates/nebula-sandbox/src/tier.rs`; move `Tier { Read=0, Sandbox=1, Workspace=2, System=3 }` (with its `Ord` derive and docs), `pub const NO_APPROVAL_THRESHOLD: Tier = Tier::Sandbox`, and the pure `effective_tier(classifier_tier, advisory_tier) -> Tier = max(..)` helper verbatim from `nebula-tools/src/permit.rs`
    - Keep identical definitions and `Ord` semantics so `effective_tier` still never drops below the classifier tier; doc comments on every public item
    - _Requirements: 8.3, 8.4, 8.5_
    - _Properties: P12_

  - [x] 2.2 Move the `CommandClassifier` trait into `nebula-sandbox`
    - Create `crates/nebula-sandbox/src/classifier.rs`; move the object-safe `pub trait CommandClassifier: Send + Sync` with the UNCHANGED signature `fn classify(&self, command: &str, args: &[String]) -> (Tier, Option<Approval>)` so it can still be stored as `Arc<dyn CommandClassifier>`
    - _Requirements: 8.1_

  - [x] 2.3 Reduce `nebula-tools/src/permit.rs` to a thin re-export module
    - Replace the moved definitions in `permit.rs` with `pub use nebula_sandbox::{Tier, NO_APPROVAL_THRESHOLD, Approval, CommandClassifier, effective_tier};` so `nebula_tools::permit::Tier` and friends keep resolving
    - Keep `DefaultClassifier` as a thin alias/constructor that builds the embedded `RulesClassifier` (stubbed to delegate for now; the real engine lands in task 5) so `nebula_tools::DefaultClassifier` references keep resolving (Requirement 8.6)
    - Make NO changes to `shell.rs`, `builtin_providers.rs`, the host, or any issue #27 call site or test
    - _Requirements: 8.1, 8.6_

  - [x] 2.4 Property test — a second opinion never lowers the enforced tier
    - **Property 12: A second opinion never lowers the enforced tier**
    - **Validates: Requirements 8.3, 8.4, 8.5**
    - `proptest` over all classifier/advisory tier pairs asserting `effective_tier` equals `max(classifier, advisory)` and is never below the classifier tier; min 100 iterations (preserves issue #27 Property 6)
    - Tag: `// Feature: permission-tiers, Property 12: a second opinion never lowers the enforced tier`
    - Place in `crates/nebula-sandbox/src/tier.rs` tests
    - _Properties: P12_

- [x] 3. Checkpoint — relocation keeps issue #27 green
  - Confirm `cargo build -p nebula-sandbox -p nebula-tools` and `cargo nextest run -p nebula-tools --locked` pass with no edits to any issue #27 test
  - Ensure all tests pass, ask the user if questions arise.

- [x] 4. Enrich the `Approval` grant contract
  - [x] 4.1 Implement `Approval`, `GrantId`, `ApprovalScope`, `ApprovalRequest`, `ApprovalRefusal`
    - Create `crates/nebula-sandbox/src/approval.rs`; define `Approval` with a private `id: GrantId` and `scope: ApprovalScope`, laid out so a future expiry field can be added without a breaking change, with NO publicly constructible authorizing field (Req 5.5)
    - Define `GrantId` (unforgeable, unique — a private `u128`), `ApprovalScope { command, tier, trace_id }` carrying command identity + authorized tier + trace/task id (Req 5.1), `ApprovalRequest { command, rule_id, tier, trace_id }` (secret-free by construction, Req 4.4, 4.5), and `ApprovalRefusal { ScopeMismatch, AlreadyConsumed }` (Req 5.3, 5.6)
    - Implement the sole `pub fn grant(id, scope) -> Approval` constructor and `authorizes(&self, command, tier, trace_id) -> bool` returning true only on a full scope match (Req 5.2); origin-agnostic, no remote field, no wall-clock expiry
    - `thiserror` for any error; doc comments on every public item; no `unwrap`/`expect`/`panic`
    - _Requirements: 4.4, 4.5, 5.1, 5.2, 5.3, 5.5, 5.6, 5.7_
    - _Properties: P9, P10_

  - [x] 4.2 Implement `ApprovalStore` (interior-mutable, keyed by trace/task id)
    - In `approval.rs`, define `ApprovalStore` holding granted, not-yet-consumed approvals keyed by trace/task id behind a `Mutex`, so a shared `Arc<dyn CommandClassifier>` can consume without `&mut self`
    - Provide `insert(approval)`, and a `take_authorizing(command, tier, trace_id) -> Result<Approval, ApprovalRefusal>` that returns and CONSUMES a single-use grant on the first authorizing match, returns `ScopeMismatch` when none matches scope, and `AlreadyConsumed` when a presented grant was already consumed (Req 5.6)
    - _Requirements: 5.2, 5.3, 5.6_
    - _Properties: P9, P10_

  - [x] 4.3 Unit tests — approval fields and the grant constructor
    - Assert `grant` populates grant id and scope; `authorizes` returns true only on a full match of command identity, tier, and trace/task id; a mismatch returns false
    - _Requirements: 5.1, 5.2_

  - [x] 4.4 Property test — only a scope-matching, unconsumed approval authorizes; a refusal emits a secret-free request
    - **Property 9: Only a scope-matching, unconsumed approval authorizes; a refusal emits a secret-free request**
    - **Validates: Requirements 4.1, 4.2, 4.4, 5.2, 5.3, 5.6**
    - `proptest` over generated approvals and presented commands: authorize iff full scope match AND unconsumed; scope mismatch → `ScopeMismatch`; second authorizing use → `AlreadyConsumed`; each refusal leaves the command unexecuted; an above-threshold command with no authorizing approval emits an `ApprovalRequest` carrying command name, matched rule id, and tier and NO secret value; min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 9: only a scope-matching, unconsumed approval authorizes`
    - _Properties: P9_

  - [x] 4.5 Property test — an approval cannot be forged to authorize
    - **Property 10: An approval cannot be forged to authorize**
    - **Validates: Requirements 5.5**
    - `proptest` asserting authorization only ever succeeds for `grant`-constructed approvals; there is no other public constructor and no publicly settable authorizing field (compile-time/structural assertion exercised over generated scopes); min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 10: an approval cannot be forged to authorize`
    - _Properties: P10_

- [x] 5. Rules table: serde structs, loader, validation, and the embedded data
  - [x] 5.1 Define the rules-table serde structs and `LoadError`
    - Create `crates/nebula-sandbox/src/rules.rs`; define `RuleId` (newtype over `String`, `#[serde(transparent)]`), `RulesFile { rule: Vec<Rule> }`, `Rule { id, #[serde(rename="match")] condition, tier: u8 }`, `MatchCondition { command, args_any, args_all, encoded_indirection, network_config, path }`, `PathCondition { targets, destructive }`, and `PathTargets` — all `#[serde(deny_unknown_fields)]` with `#[serde(default)]` where appropriate
    - Define `LoadError` (`thiserror`): `Parse(String)`, `DuplicateRuleId(RuleId)`, `MalformedRule { id, detail }`, `RedTeamFixture(String)`
    - Doc comments on every public item
    - _Requirements: 1.3, 1.8_
    - _Properties: P7_

  - [x] 5.2 Implement `RulesTable::load` with validation and the command index
    - In `rules.rs`, implement `RulesTable { rules, by_command }` and `pub fn load(toml_text: &str) -> Result<Self, LoadError>`: parse via `toml::from_str` (`Parse` on syntax error), reject the WHOLE table if any rule's `tier` is outside `0..=3` (`MalformedRule`) or two rules share an id (`DuplicateRuleId`), naming the offending entry or duplicated id (Req 1.3, 1.8)
    - Build the `by_command` index (normalized command base name → candidate rule indices) for fast matching; loading completes before any decision can be produced (Req 1.7)
    - _Requirements: 1.3, 1.7, 1.8_
    - _Properties: P7_

  - [x] 5.3 Author the embedded `data/rules.toml`
    - Create `crates/nebula-sandbox/src/data/rules.toml` with real rules covering the required behaviours: destructive recursive delete (`rm -r -f`, `Remove-Item -Recurse`, `del`), `format`, `diskpart`, registry edits (`reg`), network-config change (`netsh`, with `network_config = true`, Req 2.9), `git push` / `git reset --hard` on protected refs, encoded/indirection sinks (`powershell -EncodedCommand`/`-enc` with `encoded_indirection = true`, Req 3.4), global installs, destructive write to a path outside the worktree (`[rule.match.path] destructive = true`, Req 2.10), read-only commands (`ls`, `dir`, `cat`, `type`) → Tier 0, and sandbox writes (`mkdir`, `touch`) → Tier 1
    - Each `[[rule]]` has a stable unique `id`, a `[rule.match]` condition, and a `tier` in `0..=3`
    - _Requirements: 1.3, 2.9, 2.10, 3.4_

  - [x] 5.4 Property test — load validation accepts well-formed tables and rejects malformed or duplicate-id ones
    - **Property 7: Load validation accepts well-formed tables and rejects malformed or duplicate-id ones**
    - **Validates: Requirements 1.3, 1.8**
    - `proptest` over generated tables with valid/invalid tiers, well-formed/malformed rules, and unique/duplicate ids; assert `load` succeeds iff all rules are well-formed with tier in `0..=3` and ids are unique, otherwise returns a `LoadError` naming the offender, and no classification can be produced from a rejected table; min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 7: load validation accepts well-formed tables and rejects malformed or duplicate-id ones`
    - _Properties: P7_

  - [x] 5.5 Unit test — the embedded table loads cleanly
    - `RulesTable::load(include_str!("data/rules.toml"))` returns `Ok` and contains the expected rule ids with tiers in `0..=3`
    - _Requirements: 1.1, 1.7_

- [x] 6. The `RulesClassifier` engine
  - [x] 6.1 Implement the classifier struct and `embedded` constructor
    - Create `crates/nebula-sandbox/src/engine.rs`; define `RulesClassifier { table, worktree_root, retired_drive, approvals: Arc<ApprovalStore> }` and `pub fn embedded(worktree_root, retired_drive) -> Result<Self, LoadError>` that loads `include_str!("data/rules.toml")` via `RulesTable::load` — a classifier only exists once its table loaded cleanly (Req 1.7)
    - Hold the trace id source (read from the active `tracing` span) so `classify` stays parameter-free beyond `command`/`args`
    - _Requirements: 1.1, 1.7, 8.1_

  - [x] 6.2 Implement segmentation and encoded/indirection detection
    - In `engine.rs`, implement segmentation on any of `&&`, `||`, `;`, `|`, a newline, or a bare `&`; each segment is classified independently (Req 3.3)
    - Implement encoded/indirection detection: a segment using PowerShell `-EncodedCommand`/`-enc`, a piped download-to-shell (`curl … | sh`, `iwr … | iex`), `eval`, or environment-variable command indirection is assigned `Tier::System` directly (Req 3.4)
    - _Requirements: 3.3, 3.4_
    - _Properties: P2, P3_

  - [x] 6.3 Implement rule matching with highest-tier-wins, deterministic tie-break, and unknown→Tier 3
    - In `engine.rs`, match each segment's command name (+ aliases), argument patterns (`args_any`/`args_all`), and paths against the `RulesTable`; when several rules match, the HIGHEST tier wins (Req 1.4); break ties deterministically by sorting on `(tier desc, rule id asc)` so the recorded matched rule id is identical across runs (Req 1.5)
    - No rule matches a segment → `Tier::System` (Req 1.6); any token, flag, or path matched by no rule raises the segment tier to `Tier::System` (Req 3.1); the command tier is the MAX over all segments (Req 3.2)
    - Record the matched rule id on the resulting `Decision`
    - _Requirements: 1.2, 1.4, 1.5, 1.6, 2.1, 3.1, 3.2_
    - _Properties: P1, P2, P3, P6_

  - [x] 6.4 Implement path-condition evaluation via the Path_Resolver
    - In `engine.rs`, when a `MatchCondition` carries a `path` sub-condition, resolve the relevant argument(s) through the held `Path_Resolver` (`path::resolve(candidate, worktree_root, retired_drive)`) BEFORE evaluating the condition (Req 2.2); map results to a tier floor: `Permitted` inside the worktree → the condition's tier; `Permitted` outside → at least `Tier::Workspace` (Req 2.4); `Rejected(WorktreeEscape)` → at least `Tier::Workspace` (Req 2.5); `Rejected(RetiredDrive)` → `Tier::System` (Req 2.6); resolver `io::Error` → `Tier::System` (Req 2.7)
    - When a command names more than one target path, resolve each individually and take the HIGHEST resulting tier (Req 2.3); a destructive op on a resolved path outside the worktree → `Tier::System` (Req 2.10); the classifier itself performs no filesystem I/O beyond the resolver call
    - _Requirements: 2.2, 2.3, 2.4, 2.5, 2.6, 2.7, 2.10_
    - _Properties: P4_

  - [x] 6.5 Implement `classify` and `classify_with_magnitude`, wiring approval gating
    - Implement `impl CommandClassifier for RulesClassifier` with the unchanged `classify(&self, command, args) -> (Tier, Option<Approval>)`: run the pipeline (segment → encoded-detect → match → path floors → segment-max), then for a tier above `NO_APPROVAL_THRESHOLD` look up an authorizing, unconsumed, scope-valid approval from the held `ApprovalStore` by trace/task id — consume and return `Some(Approval)` when present, otherwise return `None` (Req 4.1, 4.2, 4.3)
    - Implement `pub fn classify_with_magnitude(&self, command, args, DeleteMagnitude { bytes, files })`: raise the decision to `Tier::System` when `bytes > 1073741824` or `files > 500`, otherwise identical to `classify` (Req 2.8); `classify` stays pure and fast (≤ 50 ms, Req 8.2)
    - _Requirements: 2.8, 4.1, 4.2, 4.3, 8.1, 8.2_
    - _Properties: P1, P5_

  - [x] 6.6 Property test — monotonicity: adding or obscuring parts never lowers the tier
    - **Property 1: Monotonicity — adding or obscuring parts never lowers the tier**
    - **Validates: Requirements 3.1, 3.2**
    - `proptest` generating a base command and an extension (extra tokens/flags/paths/segments, or obscured parts); assert `classify(extended).tier >= classify(base).tier`; min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 1: monotonicity — adding or obscuring parts never lowers the tier`
    - _Properties: P1_

  - [x] 6.7 Property test — unknown or unevaluable parts force Tier 3
    - **Property 2: Unknown or unevaluable parts force Tier 3**
    - **Validates: Requirements 1.6, 3.1, 3.4**
    - `proptest` injecting an unknown token/flag/path, a no-match command, or an encoding/indirection form (`-EncodedCommand`/`-enc`, `curl … | sh`, `iwr … | iex`, `eval`, env-var command indirection); assert `Tier::System`; min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 2: unknown or unevaluable parts force Tier 3`
    - _Properties: P2_

  - [x] 6.8 Property test — a segmented command's tier is the maximum over its segments
    - **Property 3: A segmented command's tier is the maximum over its segments**
    - **Validates: Requirements 3.2, 3.3**
    - `proptest` joining generated segments with every separator (`&&`, `||`, `;`, `|`, newline, bare `&`); assert the tier equals the max segment tier and is never lower than any segment's tier; min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 3: a segmented command's tier is the maximum over its segments`
    - _Properties: P3_

  - [x] 6.9 Property test — a resolved path maps to the correct tier floor
    - **Property 4: A resolved path maps to the correct tier floor**
    - **Validates: Requirements 2.2, 2.3, 2.4, 2.5, 2.6, 2.7, 2.10**
    - `proptest` over target paths against tempdir worktrees that resolve `Permitted`-inside, `Permitted`-outside, `WorktreeEscape`, `RetiredDrive`, and an injected resolver I/O error (plus multi-path and destructive-outside cases); assert the tier floor per the mapping table; min 100 iterations; never touch real paths or `C:`
    - Tag: `// Feature: permission-tiers, Property 4: a resolved path maps to the correct tier floor`
    - _Properties: P4_

  - [x] 6.10 Property test — delete magnitude over the limit forces Tier 3
    - **Property 5: Delete magnitude over the limit forces Tier 3**
    - **Validates: Requirements 2.8**
    - `proptest` over generated `(bytes, files)`; assert `classify_with_magnitude` returns `Tier::System` iff `bytes > 1073741824` or `files > 500`, and otherwise a tier no lower than the base `classify` tier for the same command; min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 5: delete magnitude over the limit forces Tier 3`
    - _Properties: P5_

  - [x] 6.11 Property test — classification is deterministic and records a stable matched rule id
    - **Property 6: Classification is deterministic and records a stable matched rule id**
    - **Validates: Requirements 1.2, 1.4, 1.5**
    - `proptest` classifying the same input repeatedly (including across fresh loads of the same table and tie cases); assert identical tier and identical recorded matched rule id every time; min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 6: classification is deterministic and records a stable matched rule id`
    - _Properties: P6_

  - [x] 6.12 Unit tests — timing and representative tier-0/1 permits
    - Representative read/sandbox commands classify at Tier 0/1 with no approval (Req 4.3); `netsh …` classifies at Tier 3 (Req 2.9); a single `classify` on representative inputs completes well under 50 ms (Req 8.2)
    - _Requirements: 2.9, 4.3, 8.2_

- [x] 7. Decision logging
  - [x] 7.1 Emit one `permit.decision` tracing event per classification
    - Add a private `log_decision` helper in `engine.rs` called exactly once before `classify` returns, emitting one structured `tracing` event named `permit.decision` carrying the assigned `tier`, the matched `rule_id` (a stable `"<no-match>"` sentinel when no rule matched, Req 6.4), the `approval` outcome (`permitted` | `escalated` | `refused`, Req 6.2, 6.3), the `command` name, and the `trace_id` read from the active span (Req 6.1, 6.2)
    - Pass secret-bearing arguments through the `nebula-telemetry` redactor before attaching anything, so no registered secret reaches the event, mirroring how `nebula-tools` logs `tool.call` (Req 6.5); add `nebula-telemetry` as a dependency if not already present
    - _Requirements: 6.1, 6.2, 6.3, 6.4, 6.5_
    - _Properties: P11_

  - [x] 7.2 Property test — exactly one decision event per classification, with required fields and no secrets
    - **Property 11: Exactly one decision event per classification, with required fields and no secrets**
    - **Validates: Requirements 4.5, 6.1, 6.2, 6.3, 6.4, 6.5**
    - `proptest` capturing emitted events over generated classifications/outcomes (including secret-bearing arguments and no-match cases); assert exactly one `permit.decision` per classify carrying the required fields (tier, matched rule id or no-match sentinel, approval outcome, command, trace id) with no registered secret present; min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 11: exactly one decision event per classification`
    - _Properties: P11_

- [x] 8. Red-team fixture and the Red_Team_Test
  - [x] 8.1 Author `data/red_team.toml` and its loader
    - Create `crates/nebula-sandbox/src/data/red_team.toml` with at least 50 `[[entry]]` entries, each pairing exactly one `command` string with exactly one `expected_tier` and a `category` (Req 7.1); include at least one entry in EACH category: deletes, `format`, `diskpart`, registry edits, `git push --force`, `curl | sh`, encoded PowerShell (`-EncodedCommand`), path tricks (`..`, symlinks, absolute paths, `C:`), and chained/piped commands (Req 7.2)
    - In `crates/nebula-sandbox/src/redteam.rs`, define `RedTeamFile { entry: Vec<RedTeamEntry> }` and `RedTeamEntry { command, expected_tier, category }` (`#[serde(deny_unknown_fields)]`) and a loader over the embedded fixture that fails with `LoadError::RedTeamFixture` on a malformed fixture
    - _Requirements: 7.1, 7.2_
    - _Properties: P8_

  - [x] 8.2 Implement the Red_Team_Test
    - In `redteam.rs` tests, evaluate every fixture entry with the embedded classifier: assert each entry's assigned tier equals its recorded `expected_tier`, failing with EACH mismatched command plus its expected and assigned tier (Req 7.3, 7.4); assert every entry whose expected tier is Tier 3 yields no approval so `shell.run` would refuse it pending an approval (Req 7.5); assert the fixture has ≥50 entries and at least one entry per required category, failing with the unmet count or missing category otherwise (Req 7.6)
    - _Requirements: 7.3, 7.4, 7.5, 7.6_
    - _Properties: P8_

  - [x] 8.3 Property test — every red-team entry classifies at its recorded tier and Tier-3 entries refuse without approval
    - **Property 8: Every red-team entry classifies at its recorded tier and Tier-3 entries refuse without approval**
    - **Validates: Requirements 7.1, 7.2, 7.3, 7.5, 7.6**
    - `proptest` iterating the checked-in fixture; assert each entry's tier equals its expected tier, every Tier-3 entry yields no approval, and the fixture meets the count/category requirements, reporting all mismatches; min 100 iterations
    - Tag: `// Feature: permission-tiers, Property 8: every red-team entry classifies at its recorded tier`
    - _Properties: P8_

- [x] 9. Daemon wiring and the optional config key
  - [x] 9.1 Swap `DefaultClassifier` → `RulesClassifier::embedded` in the daemon
    - In `nebula-daemon/src/builtin_providers.rs`, replace `Arc::new(DefaultClassifier)` with `Arc::new(RulesClassifier::embedded(worktree_root, retired_drive)?)`, mapping a `LoadError` to a daemon STARTUP FAILURE (a protected-set misconfiguration must not boot into an unprotected state)
    - Pass the configured worktree root and `resources.retired_drive` (existing config, default `"C:"`); the advisory-tier merge stays in `nebula-tools` as the re-exported `effective_tier`; leave `shell.run` call sites unchanged (Req 8.3–8.5)
    - _Requirements: 1.7, 8.1, 8.6_

  - [x] 9.2 Add the optional `[sandbox] rules_table_path` config key
    - Add `[sandbox] rules_table_path = ""` (empty = use the embedded table) to `config/default.toml` with a doc comment noting that editing the embedded table requires a rebuild/reinstall (the AGENTS.md lock-file gotcha)
    - Mirror the key in `nebula-config` as a typed field with `#[serde(deny_unknown_fields)]` and a doc comment; empty string resolves to the embedded table
    - _Requirements: 1.1_

  - [x] 9.3 Unit tests — config parse and daemon startup failure on bad table
    - Default config parses with `rules_table_path = ""`; an unknown key under `[sandbox]` is rejected; a `RulesClassifier::embedded` `LoadError` surfaces as a startup failure rather than a silent boot
    - _Requirements: 1.1, 1.7_

- [x] 10. Checkpoint — sandbox, tools, and daemon build and all tests pass
  - Confirm `cargo build -p nebula-sandbox -p nebula-tools -p nebula-daemon` and `cargo nextest run --workspace --locked` pass; the issue #27 `shell.run` suite passes unchanged against the swapped-in classifier (Req 8.6)
  - Ensure all tests pass, ask the user if questions arise.

- [x] 11. Documentation and final gate (protected set — never merge)
  - Add/refresh the `nebula-sandbox` crate module doc and the module docs on `tier`, `approval`, `classifier`, `rules`, `engine`, and `redteam`; update the thin `nebula-tools/src/permit.rs` re-export doc and `docs/ARCHITECTURE.md` (crate table + any permission-policy note) in the same change
  - Run the full gate: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo nextest run --workspace --locked`; confirm the bench `ruff` + `pytest` are unaffected
  - Open a pull request on a `feat/…` branch for human review and DO NOT merge it (protected-set feature, AGENTS.md hard rules 1 and 9)
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marked with `*` are optional test/validation sub-tasks and can be skipped for a faster MVP, but all 12 property tests and the issue #28 acceptance tests (the red-team fixture and its test, load validation, approval scope/single-use/unforgeability) are strongly recommended given the security-sensitive, protected-set nature of this feature.
- Each property test uses `proptest`, runs a minimum of 100 iterations, and is tagged with its `// Feature: permission-tiers, Property N: ...` comment and a `Validates: Requirements X.Y` annotation.
- Property placement is next to the code it verifies: P12 with the relocated tier vocabulary (task 2), P9/P10 with the approval contract (task 4), P7 with the rules loader (task 5), P1/P2/P3/P4/P5/P6 with the classifier engine (task 6), P11 with decision logging (task 7), and P8 with the red-team fixture (task 8).
- Only coding, testing, and documentation tasks are included; no deployment, user-testing, or operational tasks.
- The relocation (task 2) and checkpoint (task 3) land before any new engine work so issue #27 stays green throughout; the final gate (task 11) runs the full gate and opens a PR without merging.
- The classifier performs no filesystem I/O itself (it delegates to the `Path_Resolver`) and never reads or writes the retired drive; the delete-limit is honoured only via the caller-supplied `DeleteMagnitude`.

## Task Dependency Graph

```json
{
  "waves": [
    { "id": 0, "tasks": ["1.1"] },
    { "id": 1, "tasks": ["2.1", "2.2"] },
    { "id": 2, "tasks": ["2.3", "2.4", "4.1", "5.1", "5.3", "8.1"] },
    { "id": 3, "tasks": ["4.2", "4.3", "4.4", "4.5", "5.2", "5.4", "5.5"] },
    { "id": 4, "tasks": ["6.1", "6.2", "6.3", "6.4"] },
    { "id": 5, "tasks": ["6.5", "6.6", "6.7", "6.8", "6.9", "6.11", "6.12"] },
    { "id": 6, "tasks": ["6.10", "7.1", "8.2", "8.3"] },
    { "id": 7, "tasks": ["7.2", "9.1", "9.2"] },
    { "id": 8, "tasks": ["9.3"] }
  ]
}
```
