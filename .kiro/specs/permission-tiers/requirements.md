# Requirements Document

## Introduction

This feature implements the permission-tier system and command classifier for Nebula (GitHub issue #28, Phase 1 exit criterion 2). It replaces the deterministic `DefaultClassifier` placeholder shipped in issue #27 with a real, data-driven classification engine that assigns every tool action and shell command a privilege tier (0–3), enforces that tiers above the no-approval threshold wait for an explicit approval decision, and logs every decision with the rule that produced it.

The core of the feature is a **data-driven rules table** (not hardcoded Rust match arms) that maps command names, arguments, and target paths to tiers, so the required red-team coverage and future rule additions are maintainable without code changes. The engine honours a central safety invariant: a command containing any unrecognized token, flag, or path must never be classified at a lower (less restrictive) tier than it would be if those parts were known-dangerous — unknown parts default to the stricter tier.

This feature defines the approval **request and grant contract** and the enforcement path. It does **not** build the approval user interface: the TUI modal (issue #31) and the ntfy surface (issue #37) are separate issues that will consume this contract. Path confinement (worktree containment, the retired-drive rule) is owned by the existing `Path_Resolver` from issue #27 and is referenced here, not re-implemented.

This is a **protected-set / safety feature**. Per AGENTS.md hard rule 9, changes to the permission policy are always human-reviewed.

## Glossary

- **Tier**: An operation privilege level ordered 0 through 3, where a higher tier is strictly more privileged and therefore more restricted. Tier 0 = Read (read-only), Tier 1 = Sandbox (write confined to the `Worktree_Root`), Tier 2 = Workspace (workspace-level operations), Tier 3 = System (destructive, outside the worktree, network-changing, or exceeding the delete limit). Defined by the `Tier` enum in `crates/nebula-tools/src/permit.rs`.
- **No_Approval_Threshold**: The highest `Tier` that executes without an approval decision, equal to Tier 1 (Sandbox). Any command classified strictly above this threshold (Tier 2 or Tier 3) requires an approval decision. Defined as `NO_APPROVAL_THRESHOLD` in `permit.rs`.
- **Approval**: A grant that authorizes execution of a command classified above the `No_Approval_Threshold`. For this feature the `Approval` type carries grant identity and scope (and optionally expiry), superseding the opaque placeholder from issue #27.
- **Command_Classifier**: The object-safe trait `CommandClassifier` in `permit.rs`, stored as `Arc<dyn CommandClassifier>` on the tool context. Its `classify(command, args)` method returns a `(Tier, Option<Approval>)` decision. This feature provides the real implementation that supersedes `DefaultClassifier`.
- **Rules_Table**: The data (not code) that maps command names, arguments, and target paths to a `Tier` and a rule identifier. The authoritative source of classification decisions.
- **Rule**: A single entry in the `Rules_Table` with a stable identifier, a match condition (command name, argument pattern, and/or path condition), and the `Tier` it assigns. The matched `Rule` identifier is recorded on every decision.
- **Red_Team_List**: A checked-in test fixture of at least 50 adversarial commands (deletes, `format`, `diskpart`, registry edits, `git push --force`, `curl | sh`, encoded PowerShell, path tricks, chained/piped commands) used to assert classification and approval-gating behaviour.
- **Red_Team_Test**: The test that evaluates the `Red_Team_List`, asserting that each entry is classified at its recorded expected `Tier` and failing with the offending entries when it is not.
- **Retired_Drive**: The failing drive (`C:` by default, configured as `resources.retired_drive`) that must never be read from or written to. Enforced by the `Path_Resolver`; referenced by path-aware classification.
- **Worktree_Root**: The confinement root for Tier 1 (Sandbox) writes. A target path that resolves outside the `Worktree_Root` escalates the tier. Enforced by the `Path_Resolver`.
- **Path_Resolver**: The existing single path-resolution and confinement routine (`crates/nebula-tools/src/path.rs`, issue #27) that fully resolves a candidate path (expanding `.`/`..`, following symlinks and junctions, canonicalizing) and reports whether it is confined to the `Worktree_Root` and off the `Retired_Drive`.
- **Effective_Tier**: The tier `shell.run` enforces, computed as the maximum of the `Command_Classifier` tier and an advisory model second-opinion tier. An advisory input may raise but never lower the enforced tier (`effective_tier` in `permit.rs`, Property 6 of issue #27).
- **Decision**: The result of classifying one command: a `Tier`, the matched `Rule` identifier, and any `Approval` outcome. Every `Decision` is logged.

## Requirements

### Requirement 1: Data-driven rules table

**User Story:** As a Nebula maintainer, I want command classification driven by a data table rather than hardcoded logic, so that the red-team list and future rules can be maintained without changing code.

#### Acceptance Criteria

1. THE Command_Classifier SHALL determine the Tier of a command from the Rules_Table rather than from hardcoded command-name match arms.
2. WHEN the Command_Classifier classifies a command, THE Command_Classifier SHALL select the matched Rule from the Rules_Table and record its Rule identifier on the Decision.
3. THE Rules_Table SHALL associate each Rule with a Rule identifier that is unique and stable across loads of the same Rules_Table, a match condition, and an assigned Tier in the range Tier 0 (Read) through Tier 3 (System).
4. WHEN two or more Rules match one command, THE Command_Classifier SHALL apply the Rule that assigns the highest Tier.
5. WHEN two or more matching Rules assign the same highest Tier, THE Command_Classifier SHALL select exactly one of those Rules by a deterministic tie-break so that the recorded matched Rule identifier is identical for identical inputs across runs.
6. IF a command matches no Rule in the Rules_Table, THEN THE Command_Classifier SHALL assign Tier 3 (System).
7. WHEN the Command_Classifier classifies a command, THE Command_Classifier SHALL have completed loading the Rules_Table before producing any Decision.
8. IF the Rules_Table contains a malformed Rule entry or two or more Rules sharing the same Rule identifier when it is loaded, THEN THE Command_Classifier SHALL reject the Rules_Table, decline to classify any command, and surface a load error indicating the offending Rule entry or duplicated identifier.

### Requirement 2: Tier classification by command, arguments, and path

**User Story:** As a Nebula operator, I want classification to consider a command's arguments and target paths, not just its name, so that the same command is tiered correctly based on what it actually does.

#### Acceptance Criteria

1. WHEN the Command_Classifier classifies a command, THE Command_Classifier SHALL evaluate the command name, the arguments, and all target paths against the Rules_Table.
2. WHEN a Rule match condition references a target path, THE Command_Classifier SHALL resolve that path through the Path_Resolver before evaluating the condition.
3. WHEN a command specifies more than one target path, THE Command_Classifier SHALL assign the highest tier produced by evaluating each resolved path individually.
4. IF a target path resolves with status Permitted and lies outside the Worktree_Root, THEN THE Command_Classifier SHALL assign at least Tier 2 (Workspace).
5. IF the Path_Resolver reports Rejected with reason WorktreeEscape for a target path, THEN THE Command_Classifier SHALL assign at least Tier 2 (Workspace).
6. IF the Path_Resolver reports Rejected with reason RetiredDrive for a target path, THEN THE Command_Classifier SHALL assign Tier 3 (System).
7. IF a target path cannot be resolved because the Path_Resolver returns an I/O error, THEN THE Command_Classifier SHALL assign Tier 3 (System).
8. IF a command deletes a total of more than 1073741824 bytes or more than 500 files, THEN THE Command_Classifier SHALL assign Tier 3 (System).
9. WHEN a command invokes an operation that modifies network configuration, THE Command_Classifier SHALL assign Tier 3 (System).
10. WHEN a command performs a destructive operation, defined as one that deletes, overwrites, or truncates existing files, on a resolved target path outside the Worktree_Root, THE Command_Classifier SHALL assign Tier 3 (System).

### Requirement 3: Never downgrade on unknown parts (safety invariant)

**User Story:** As a Nebula owner, I want any unrecognized part of a command to make classification stricter rather than more permissive, so that an attacker cannot smuggle a dangerous operation past the classifier by obscuring it.

#### Acceptance Criteria

1. IF a command contains a token, flag, or path that matches no Rule in the Rules_Table, THEN THE Command_Classifier SHALL assign Tier 3 (System).
2. WHEN the Command_Classifier classifies a command composed of multiple sub-commands or segments, THE Command_Classifier SHALL assign a Tier no lower than the highest Tier produced for any individual segment.
3. WHEN a command is a sequence of segments joined by any of the separators `&&`, `||`, `;`, `|`, a newline, or a bare `&`, THE Command_Classifier SHALL classify each segment and assign the maximum Tier across all segments.
4. IF a command uses an encoding, indirection, or escaping that prevents full evaluation of its effect, including PowerShell `-EncodedCommand` or `-enc`, a piped download-to-shell such as `curl | sh`, `eval`, or environment-variable command indirection, THEN THE Command_Classifier SHALL assign Tier 3 (System).

### Requirement 4: Tier-3 approval gating

**User Story:** As a Nebula owner, I want every Tier-3 action to wait for my explicit approval, so that destructive or system-level operations never run without a human decision.

#### Acceptance Criteria

1. IF a command is classified above the No_Approval_Threshold and no Approval is present, THEN THE Command_Classifier SHALL refuse the command and return the refusal to the caller as an error without starting any process.
2. WHILE a command is classified at Tier 2 (Workspace) or Tier 3 (System), IF no Approval covering the command is present, THEN THE Command_Classifier SHALL leave the command unexecuted.
3. WHEN a command is classified at Tier 0 (Read) or Tier 1 (Sandbox), THE Command_Classifier SHALL permit the command without an Approval decision.
4. WHEN a command classified above the No_Approval_Threshold is refused for lack of an Approval, THE Command_Classifier SHALL emit an approval request describing the classified command name, the matched Rule identifier, and the assigned Tier.
5. THE Command_Classifier SHALL exclude secret values from every emitted approval request.

### Requirement 5: Approval grant contract

**User Story:** As a developer of the approval surfaces (TUI modal, ntfy), I want a well-defined approval request and grant contract, so that I can build an approval UI against a stable interface without this feature depending on that UI.

#### Acceptance Criteria

1. THE Approval SHALL carry a grant identity, an authorized command scope consisting of the command identity and the permission tier it authorizes, and the associated trace or task identifier.
2. WHEN an Approval is presented for a command, THE Command_Classifier SHALL authorize execution only if the command's identity, permission tier, and trace or task identifier all match the corresponding values in the Approval scope.
3. IF an Approval is presented for a command whose identity, permission tier, or trace or task identifier does not match the Approval scope, THEN THE Command_Classifier SHALL refuse the command, return a refusal result indicating a scope mismatch, and leave the command unexecuted.
4. WHERE an Approval carries an expiry, IF the Approval expiry time is at or before the current time when the command is presented, THEN THE Command_Classifier SHALL refuse the command, return a refusal result indicating that the Approval has expired, and leave the command unexecuted.
5. THE Approval SHALL be constructible only through the defined grant constructor and SHALL expose no publicly constructible authorizing fields, so that an external caller cannot construct an authorizing Approval by any other means.
6. WHERE an Approval is designated single-use, WHEN that Approval has authorized one command, THE Command_Classifier SHALL refuse any subsequent command presenting the same Approval and return a refusal result indicating the Approval has already been consumed.
7. THE Command_Classifier SHALL define the approval request and grant contract without depending on any approval user interface.

### Requirement 6: Decision logging

**User Story:** As a Nebula operator, I want every classification decision logged with the rule that matched, so that I can audit why any command was permitted, escalated, or refused.

#### Acceptance Criteria

1. WHEN the Command_Classifier produces a Decision, THE Command_Classifier SHALL emit exactly one structured log event, using the existing `tracing` event conventions used in nebula-tools, with a stable event name identifying it as a classification decision.
2. WHEN the Command_Classifier logs a Decision, THE Command_Classifier SHALL include the assigned Tier, the matched Rule identifier, the approval outcome (permitted, escalated, or refused), the classified command name, and the trace id associated with the originating request.
3. IF a command is refused for lack of an Approval, THEN THE Command_Classifier SHALL log the refusal with the matched Rule identifier and an approval outcome of refused.
4. IF no Rule matches the command, THEN THE Command_Classifier SHALL log the Decision with a Rule identifier value indicating that no rule matched.
5. THE Command_Classifier SHALL exclude all secret values registered with the telemetry redactor from every logged Decision, including secret-bearing command arguments.

### Requirement 7: Red-team coverage

**User Story:** As a Nebula owner, I want a large checked-in list of adversarial commands verified by tests, so that regressions in classification are caught before they reach my machine.

#### Acceptance Criteria

1. THE Red_Team_List SHALL be a checked-in test fixture containing at least 50 adversarial command entries, where each entry pairs exactly one command string with exactly one expected Tier value.
2. THE Red_Team_List SHALL include at least one entry in each of the following categories: deletes, `format`, `diskpart`, registry edits, `git push --force`, `curl | sh`, encoded PowerShell (`-EncodedCommand`), path tricks (`..`, symlinks, absolute paths, `C:`), and chained or piped commands.
3. WHEN the Red_Team_List is evaluated, THE Command_Classifier SHALL assign each entry a Tier equal to the expected Tier recorded for that entry in the fixture.
4. IF the Command_Classifier assigns any entry a Tier that differs from the entry's recorded expected Tier, THEN THE Red_Team_Test SHALL fail and SHALL identify each mismatched command together with its expected Tier and assigned Tier.
5. WHILE an entry's recorded expected Tier is Tier-3, THE Command_Classifier SHALL refuse the entry pending an Approval.
6. IF the Red_Team_List contains fewer than 50 entries, or omits any category listed in criterion 2, THEN THE Red_Team_Test SHALL fail and SHALL indicate the unmet count or missing category.

### Requirement 8: Backward-compatible replacement of DefaultClassifier

**User Story:** As a maintainer of the built-in tools from issue #27, I want the new classifier to drop into the existing `CommandClassifier` seam, so that `shell.run` keeps working without changes to its call sites.

#### Acceptance Criteria

1. THE Command_Classifier SHALL implement the existing `CommandClassifier` trait with the unchanged signature `classify(&self, command: &str, args: &[String]) -> (Tier, Option<Approval>)` so that it can be stored as `Arc<dyn CommandClassifier>` on the tool context without modifying any issue #27 call site.
2. WHEN `shell.run` invokes `classify` on the Command_Classifier, THE Command_Classifier SHALL return a `(Tier, Option<Approval>)` Decision within 50 milliseconds for a single invocation.
3. THE System SHALL compute the enforced tier as the maximum of the Command_Classifier tier and the advisory model second-opinion tier, where tier ordering is Tier 0 (lowest) through Tier 3 (highest).
4. WHEN an advisory model second-opinion tier is provided, THE System SHALL set the enforced tier to the higher of the Command_Classifier tier and the advisory tier, and SHALL never set the enforced tier below the Command_Classifier tier.
5. WHEN no advisory model second-opinion tier is provided, THE System SHALL set the enforced tier equal to the Command_Classifier tier.
6. IF the Command_Classifier is substituted for the previously wired DefaultClassifier, THEN THE System SHALL cause all existing issue #27 `shell.run` tests to pass with no changes to their test code.

## Open Questions

1. **Rules-table format.** Should the Rules_Table be an embedded data file (TOML or RON compiled in via `include_str!`, consistent with the existing lock files) or a typed Rust table constructed at startup? A data file keeps the red-team rules editable without recompiling but adds a parse step; a typed table is simpler but closer to code. Which do you prefer?
2. **Crate location.** The `permit` module doc flags `permit.rs` as a candidate to relocate to a new `nebula-sandbox` crate when #28 lands. Should the real engine stay in `nebula-tools` or move to a dedicated `nebula-sandbox` crate?
3. **Remote approval.** Issue #37 (ntfy) open question 32 asks whether Tier-3 approvals can be granted from the phone. Should the Approval grant contract reserve support for a remote (off-machine) approval origin now, or defer that entirely to #37?
4. **Delete-limit measurement.** The 1 GB / 500 file Tier-3 trigger (AGENTS.md hard rule 4) may require inspecting the filesystem before a delete runs. Should the classifier compute the delete size itself, or require the caller (the delete built-in) to supply the measured size and count for classification?
5. **Approval expiry.** Should Approvals carry an expiry at all in this feature, or is single-use (consumed on first authorizing match) sufficient for Phase 1?
