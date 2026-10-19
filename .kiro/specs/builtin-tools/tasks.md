# Implementation Plan: Built-in Tools (fs.\*, git.\*, shell.run, system.resources)

## Overview

This plan implements the in-process built-in tool half of `nebula-tools` (GitHub issue #27, design 6.1), extending the existing external MCP tool host from PR #42. It is a protected-set, security-sensitive feature, so tasks are strictly incremental and test-driven: shared foundations land first (error types, the `BuiltinTool` trait, the registry `ToolKind`, and the two-arm `call_inner` boundary proven by a trivial fake builtin), then the security-critical `Path_Resolver` and output cap, then the permission layer, then each tool, then registration/uniqueness and the exactly-one-event guarantee, then config, then daemon wiring and integration tests, and finally docs plus the full gate and a (never-merged) PR.

The design defines 10 correctness properties (P1–P10). Each is implemented as a single `proptest` property test (minimum 100 iterations), placed next to the code it verifies, and tagged `// Feature: builtin-tools, Property N: ...`. The issue #27 acceptance unit tests (fs path-escape attempts; `shell.run` refusing tier-3 without approval; output-over-cap truncation + blob) are included as explicit tasks.

Language: Rust (as used throughout the design and the existing `nebula-tools` crate). Conventions enforced per task: `thiserror` in libs, no `unwrap`/`expect`/`panic` in lib code (clippy deny), doc comments on public items, `async-trait` for object-safe async traits, `unsafe` only in the isolated `job` module with SAFETY comments, config keys in BOTH `config/default.toml` and `nebula-config`, and all tests redirect paths to tempdirs and never touch real paths. Target: Windows MSVC.

## Tasks

- [x] 1. Add workspace test dependencies for property testing
  - Add `proptest` to `[workspace.dependencies]` in the root `Cargo.toml` with a pinned version (the workspace has no `proptest` entry today; `insta` exists but is not a property-testing library)
  - Add `proptest` and `tempfile` to the `nebula-tools` crate `[dev-dependencies]` referencing the workspace version (tests need real temp worktrees/junctions)
  - Do not add any new runtime deps; `jsonschema` and `nebula-proto` are already present on the crate
  - _Requirements: 7.2_
  - _Properties: P1, P2, P3, P4, P6, P7, P8, P9, P10_

- [x] 2. Shared error and output foundations
  - [x] 2.1 Extend `ToolError` in lib.rs
    - Add `DuplicateTool { name: String }` variant (collision across both maps) with a `thiserror` message identifying the conflicting name
    - Extend the existing `OutputTooLarge` variant with a `prefix: String` field (keep `got`/`cap`), for the blob-write-failure diagnostic
    - Re-export the new/changed public types from `lib.rs`
    - _Requirements: 1.12, 5.6_
    - _Properties: P3, P10_

  - [x] 2.2 Define `BuiltinTool` trait, `ToolOutput`, `ToolContext`, `BuiltinLimits` in builtins/mod.rs
    - Create `crates/nebula-tools/src/builtins/mod.rs`; wire `mod builtins;` into `lib.rs`
    - Define object-safe `#[async_trait] pub trait BuiltinTool: Send + Sync` with `name`, `description` (default `None`), `input_schema`, `tier`, and cancellation-safe `call(args, &ToolContext) -> Result<ToolOutput, ToolError>`; document the cancellation/drop-guard contract
    - Define `ToolOutput { bytes: Vec<u8>, is_error: bool }` with `text(..)` and `json(&Value) -> Result<_, ToolError>` helpers
    - Define `ToolContext { worktree, classifier, resources, limits }` (cheaply cloned `Arc`s) and `BuiltinLimits { call_timeout, output_cap }`
    - Declare `WorktreeRootProvider` and `ResourceProvider` traits (the latter returns `Option<nebula_proto::ResourceSnapshot>`); leave `register_all` as a stub to be filled in task 11
    - Doc comments on every public item; no `unwrap`/`expect`/`panic`
    - _Requirements: 1.1, 1.6, 1.8, 6.1_
    - _Properties: P9_

  - [x] 2.3 Unit test `ToolOutput` helpers
    - Assert `text`/`json` produce the expected bytes and `is_error` flag; `json` serialization error maps to `ToolError`
    - _Requirements: 6.2_

- [x] 3. Registry `ToolKind` and the two-arm call boundary (proven by a fake builtin)
  - [x] 3.1 Add `ToolKind` to the registry entry in host.rs
    - Add `enum ToolKind { External { server: String }, Builtin(Arc<dyn BuiltinTool>) }`; move the external `server` onto `ToolKind::External`
    - Store `kind`, `descriptor` (with `server = "builtin"` for built-ins), and the compiled `validator` on each entry
    - Keep `list_tools` shape unchanged and sorted by name; built-ins now appear from their stored descriptors
    - _Requirements: 1.3_
    - _Properties: P9_

  - [x] 3.2 Add `register_builtin` with name/schema validation and cross-map collision in host.rs
    - Validate name length `1..=128` and a non-empty, compilable input schema; on failure return `ToolError::InvalidArguments` and do not register (Req 1.2)
    - Return `ToolError::DuplicateTool { name }` if the name is already held by a built-in OR external tool, leaving the existing registration intact (Req 1.12)
    - Compile and store the `jsonschema::Validator`; make the tool resolvable by exact `name()`
    - Tighten `register_tool`/`add_server` so an external name colliding with any existing tool returns `DuplicateTool` (surfaced per server, logged and skipped) instead of warn-and-shadow
    - _Requirements: 1.1, 1.2, 1.12_
    - _Properties: P9, P10_

  - [x] 3.3 Add the two-arm `call_inner` match and keep a single `log_call` site in host.rs
    - In `call_inner`, after resolving the entry and validating arguments against its schema (shared for both kinds), branch on `entry.kind`: External keeps the existing `McpClient` path; Builtin wraps `tool.call(args, &ctx)` in `tokio::time::timeout(limits.call_timeout, ..)`
    - On timeout elapse, drop the future (cooperative cancellation) and return `ToolError::Timeout { tool, timeout_ms }`
    - On unknown name in neither map return `ToolError::UnknownTool` (Req 1.11); keep exactly one `log_call` call site wrapping `call_inner` so every outcome emits exactly one event
    - Thread a `ToolContext` into the host so built-ins can be called (store it at construction/registration time)
    - _Requirements: 1.4, 1.5, 1.6, 1.7, 1.10, 1.11_
    - _Properties: P5, P10_

  - [x] 3.4 Unit test the builtin boundary with a trivial fake builtin
    - Add a tiny in-crate fake `BuiltinTool` (under `#[cfg(test)]` or the `test-support` feature) and register it; assert it appears in `list_tools`, is callable via the host, that invalid arguments return `InvalidArguments` without invoking it, and that an unknown name returns `UnknownTool`
    - This proves the external/builtin boundary before any real tool exists
    - _Requirements: 1.3, 1.4, 1.5, 1.11_

- [x] 4. Path resolver (security-critical confinement)
  - [x] 4.1 Implement `path::resolve` in path.rs
    - Create `crates/nebula-tools/src/path.rs`; wire `mod path;` into `lib.rs`
    - Define `RejectReason { WorktreeEscape, RetiredDrive }` and `Resolved { Permitted(PathBuf), Rejected(RejectReason) }`
    - Implement `resolve(candidate, worktree_root, retired_drive) -> io::Result<Resolved>`: canonicalize the worktree root once; make the candidate absolute (relative joins the root); canonicalize to resolve `.`/`..`, symlinks, and NTFS junctions; for non-existent paths resolve the nearest existing ancestor and rejoin lexically normalized remaining segments (Req 7.3)
    - Apply the retired-drive check FIRST (case-insensitive drive-letter compare) so it takes precedence over worktree-escape (Req 2.9, 7.6), then a component-wise containment check against the canonical root (not raw string prefix), stripping a leading `\\?\` on both sides
    - No `unwrap`/`expect`/`panic`; doc comments on all public items
    - _Requirements: 7.1, 7.2, 7.3, 7.4, 7.5, 7.6_
    - _Properties: P1, P2_

  - [x] 4.2 Property test — no permitted path escapes confinement
    - **Property 1: No permitted path escapes confinement**
    - **Validates: Requirements 2.2, 2.7, 7.1, 7.2, 7.3, 7.4, 7.5, 7.7**
    - `proptest` over generated worktree roots and adversarial candidates (`..` chains, absolute paths outside the root, symlink/junction targets built in tempdirs pointing outside); assert `Permitted(p)` implies component-wise containment and off the retired drive; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 1: no permitted path escapes confinement`
    - _Properties: P1_

  - [x] 4.3 Property test — retired-drive precedence and distinct reason
    - **Property 2: Retired-drive rejection takes precedence and carries a distinct reason**
    - **Validates: Requirements 2.8, 2.9, 7.6**
    - `proptest` over candidates resolving onto the retired drive (including also-outside-worktree, read operations, and link/junction targets); assert `Rejected(RetiredDrive)`, distinct from `WorktreeEscape`, case-insensitive; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 2: retired-drive rejection takes precedence`
    - _Properties: P2_

- [x] 5. Output cap redesign (truncate + marker + blob)
  - [x] 5.1 Implement `enforce_cap` and `TRUNCATION_MARKER` in cap.rs
    - Create `crates/nebula-tools/src/cap.rs`; wire `mod cap;` into `lib.rs`
    - Define the fixed `pub const TRUNCATION_MARKER: &str` and `enforce_cap(tool, output, cap, blobs) -> Result<(ToolCallResult, Option<String>), ToolError>`
    - `len <= cap` → return whole, no blob (Req 5.2); `len > cap` → write FULL untruncated bytes to the blob store first (Req 5.3), truncate the prefix at a UTF-8 char boundary so `prefix + marker` stays within budget (walk back until `str::from_utf8` succeeds), append the marker (Req 5.4), return the blob ref for logging (Req 5.5)
    - On blob-write failure return `ToolError::OutputTooLarge { tool, got, cap, prefix }` carrying the UTF-8-safe prefix; no success, no blob ref (Req 5.6)
    - Update `call_inner` to apply `enforce_cap` to builtin output and route external results through the same `enforce_cap` (behavior change from PR #42 reject-on-cap, called out in design); change `log_call` to take the precomputed `output_blob` so it does not re-`put` the inline content
    - _Requirements: 5.1, 5.2, 5.3, 5.4, 5.5, 5.6, 5.7, 1.8, 1.9_
    - _Properties: P3_

  - [x] 5.2 Property test — output cap truncates within bounds and preserves full output
    - **Property 3: Output cap truncates within bounds and preserves the full output**
    - **Validates: Requirements 1.8, 1.9, 2.12, 3.16, 4.10, 5.1, 5.2, 5.3, 5.4, 5.5, 5.6, 5.7**
    - `proptest` over generated byte outputs (including multibyte UTF-8 straddling the cap) and caps, with both a working blob store and an injected failing blob store; assert the invariants above; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 3: output cap truncates within bounds`
    - _Properties: P3_

  - [x] 5.3 Unit test — output-over-cap truncation + blob (issue #27 acceptance)
    - Assert output just over the cap is truncated to a UTF-8 boundary, ends with the exact `TRUNCATION_MARKER` bytes, the full output lands in the blob, and the blob ref is recorded; assert the under-cap path stores no blob
    - _Requirements: 5.2, 5.3, 5.4, 5.5_

- [x] 6. Permission types, classifier, and providers
  - [x] 6.1 Implement `permit.rs` (Tier, Approval, CommandClassifier, DefaultClassifier)
    - Create `crates/nebula-tools/src/permit.rs`; wire `mod permit;` into `lib.rs`
    - Define `Tier { Read=0, Sandbox=1, Workspace=2, System=3 }` (Ord), `pub const NO_APPROVAL_THRESHOLD: Tier = Tier::Sandbox`, `Approval` (minimal grant), and object-safe `CommandClassifier { classify(command, args) -> (Tier, Option<Approval>) }`
    - Implement `DefaultClassifier` deterministic rules: destructive file ops / `git push` / `git reset --hard` on protected refs / system config / global installs → Tier 3; unknown → Tier 2; read/sandbox → Tier 0/1; never returns an `Approval` on its own
    - Document `permit` as a candidate for relocation to `nebula-sandbox` in #28
    - _Requirements: 4.2, 4.11, 4.12_
    - _Properties: P6_

  - [x] 6.2 Implement the effective-tier helper used by `shell.run`
    - Add a pure `effective_tier(classifier_tier, advisory_tier) -> Tier` = `max(..)` that never drops below the classifier tier (second opinion advisory only)
    - _Requirements: 4.12_
    - _Properties: P6_

  - [x] 6.3 Property test — a second opinion never lowers the enforced tier
    - **Property 6: A second opinion never lowers the enforced tier**
    - **Validates: Requirements 4.12**
    - `proptest` over all classifier/advisory tier pairs asserting the enforced tier `>=` classifier tier; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 6: a second opinion never lowers the enforced tier`
    - _Properties: P6_

- [x] 7. File tools
  - [x] 7.1 Implement fs tools in builtins/fs.rs
    - Create `crates/nebula-tools/src/builtins/fs.rs`; implement `fs.read`, `fs.write`, `fs.list`, `fs.search` as `BuiltinTool`s with non-empty input schemas
    - Resolve each path through `path::resolve` before any I/O; `fs.write` resolves the PARENT directory for a new file (Req 2.2); map `Rejected(_)` → `InvalidArguments`, performing no I/O and leaving the filesystem unchanged (Req 2.7, 2.8, 7.7)
    - Tiers: `fs.read`/`fs.list`/`fs.search` = Tier 0, `fs.write` = Tier 1 (independent of any path's validity); `fs.read`/`fs.search` output flows through the cap
    - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5, 2.6, 2.7, 2.8, 2.9, 2.10, 2.11, 2.12_
    - _Properties: P1, P2, P3_

  - [x] 7.2 Unit tests — fs path-escape attempts (issue #27 acceptance)
    - Over real temp worktrees on Windows: `..` traversal, absolute path outside the worktree, a symlink and an NTFS junction (via the Win32 API) whose target is outside the worktree, and `C:` paths — each asserts `InvalidArguments` and an unchanged filesystem; also assert permitted read/write/list/search happy paths
    - All paths redirected to tempdirs; never touch real user paths
    - _Requirements: 2.3, 2.4, 2.5, 2.6, 2.7, 2.8_

- [x] 8. Git tools
  - [x] 8.1 Implement git tools in builtins/git.rs
    - Create `crates/nebula-tools/src/builtins/git.rs`; implement `git.status`, `git.diff`, `git.commit`, `git.branch`
    - Shell out to the `git` CLI with `current_dir(worktree_root)` through an internal `run_git(args, ctx)` helper that reuses the shared Job-Object child mechanism (task 9.1); capture and cap output
    - Confine to the worktree repo only (Req 3.2); input schemas expose no push/fetch/pull; any remote-mutating request → `InvalidArguments` with no remote contact (Req 3.4, 3.5)
    - Missing repo → `Unavailable` (Req 3.3); other git failure → `Unavailable`, no partial state (Req 3.13); `git.commit` empty/whitespace message → `InvalidArguments` (Req 3.9), no staged changes → `Unavailable` (Req 3.10); `git.branch create` invalid (`git check-ref-format --branch`) or existing name → `InvalidArguments` (Req 3.12)
    - Tiers: `git.status`/`git.diff` = Tier 0; `git.commit`/`git.branch create` = Tier 1; `git.diff` output flows through the cap
    - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7, 3.8, 3.9, 3.10, 3.11, 3.12, 3.13, 3.14, 3.15, 3.16_
    - _Properties: P3_

  - [x] 8.2 Unit tests — git examples
    - Over real temp repos: dirty-repo status/diff; commit of staged changes advances HEAD; no staged changes → `Unavailable`; missing message → `InvalidArguments`; branch list/create; invalid/existing branch → `InvalidArguments`; non-repo dir → `Unavailable`; push-like request refused with no remote contact
    - _Requirements: 3.3, 3.5, 3.6, 3.7, 3.8, 3.9, 3.10, 3.11, 3.12, 3.13_

- [x] 9. Shell tool with Job Object isolation
  - [x] 9.1 Promote the launcher `job` module to `pub(crate)` and share it
    - Change `mod job` in launcher.rs to `pub(crate) mod job` so `shell.run` and `run_git` reuse the one audited `unsafe` Win32 surface rather than duplicating it; keep SAFETY comments intact
    - Expose a small crate-internal child/job API (spawn in job, wait-with-timeout, kill-on-close guard) usable by shell and git
    - _Requirements: 4.6_

  - [x] 9.2 Implement `shell.run` in builtins/shell.rs
    - Create `crates/nebula-tools/src/builtins/shell.rs`; implement `shell.run { command, args? }` as a `BuiltinTool`
    - Classify via `ctx.classifier.classify(..)` BEFORE starting any child (Req 4.2, 4.11); `tier <= NO_APPROVAL_THRESHOLD` → execute (Req 4.3); `tier > threshold` with no approval → refuse with `InvalidArguments` indicating approval required, starting zero processes (Req 4.4); `tier > threshold` with approval → execute (Req 4.5); apply `effective_tier` for any advisory second opinion (Req 4.12)
    - Execution: assign the child to the shared kill-on-close Job Object (Req 4.6); launch with a Scrubbed_Environment (secret-bearing vars removed, controlled PATH) and `current_dir = worktree_root` (Req 4.7); enforce the per-call timeout from child start, killing via the job on elapse and returning `Timeout` (Req 4.8, 4.9); cap cumulative output (Req 4.10)
    - Implement a pure `scrub_env(parent_env) -> env` helper for the environment rules
    - _Requirements: 4.1, 4.2, 4.3, 4.4, 4.5, 4.6, 4.7, 4.8, 4.9, 4.10, 4.11, 4.12_
    - _Properties: P4, P7_

  - [x] 9.3 Property test — above-threshold command without approval never starts a process
    - **Property 4: An above-threshold command without approval never starts a process**
    - **Validates: Requirements 4.2, 4.4, 4.11**
    - `proptest` over generated commands with a classifier returning tier > threshold and no approval; a spy launcher records every spawn; assert zero spawns and `InvalidArguments`; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 4: above-threshold command without approval never starts a process`
    - _Properties: P4_

  - [x] 9.4 Property test — scrubbed environment removes secrets and pins PATH and cwd
    - **Property 7: The scrubbed environment removes secrets and pins PATH and cwd**
    - **Validates: Requirements 4.7**
    - `proptest` over generated parent environments asserting no secret-bearing variable survives (known names + secret patterns), PATH is the controlled value, and cwd is the worktree root; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 7: the scrubbed environment removes secrets and pins PATH and cwd`
    - _Properties: P7_

  - [x] 9.5 Unit tests — shell timeout/job + tier-3 refusal (issue #27 acceptance)
    - A sleeping command hits the timeout and is killed via the job, returning `Timeout`; `shell.run` refuses a tier-3 command without approval (`InvalidArguments`, no process) and executes it when an approving classifier is injected
    - _Requirements: 4.4, 4.8, 4.9_

- [x] 10. Resources tool
  - [x] 10.1 Implement `system.resources` in builtins/resources.rs
    - Create `crates/nebula-tools/src/builtins/resources.rs`; implement `system.resources {}` calling `ctx.resources.latest()`: `Some(snapshot)` → `ToolOutput::json(&snapshot)` (Req 6.2, 6.4); `None` → `ToolError::Unavailable { server: "builtin", detail: "no resource snapshot yet" }` (Req 6.3); Tier 0 (Req 6.5)
    - _Requirements: 6.1, 6.2, 6.3, 6.4, 6.5_
    - _Properties: P8_

  - [x] 10.2 Property test — `system.resources` output equals the snapshot field-for-field
    - **Property 8: `system.resources` output equals the snapshot field-for-field**
    - **Validates: Requirements 6.2, 6.4**
    - `proptest` over generated `ResourceSnapshot` values round-tripped through `system.resources` (serialize/deserialize), asserting field-for-field equality; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 8: system.resources output equals the snapshot field-for-field`
    - _Properties: P8_

  - [x] 10.3 Unit test — resources provider Some/None
    - Provider `Some` returns the snapshot; `None` → `Unavailable`
    - _Requirements: 6.2, 6.3_

- [x] 11. Registration wiring, uniqueness, and exactly-one-event guarantees
  - [x] 11.1 Implement `builtins::register_all`
    - Fill in `register_all(&mut host, ctx: ToolContext)` to register all ten built-ins (fs.\*, git.\*, shell.run, system.resources) with their declared tiers in one call, propagating any `register_builtin` error
    - _Requirements: 2.1, 2.10, 2.11, 3.1, 3.14, 3.15, 4.1, 6.1, 6.5_
    - _Properties: P9, P10_

  - [x] 11.2 Unit test — registration smoke
    - `register_all` registers exactly the ten tool names with their declared tiers and they appear in `list_tools` sorted by name
    - _Requirements: 2.1, 2.10, 2.11, 3.1, 3.14, 3.15, 4.1, 6.1, 6.5_

  - [x] 11.3 Property test — registration validation accepts valid and rejects invalid
    - **Property 9: Registration validation accepts valid tools and rejects invalid ones**
    - **Validates: Requirements 1.1, 1.2, 1.3**
    - `proptest` over generated names (valid, over-length, empty) and schemas (non-empty/compilable vs empty/uncompilable); valid → resolvable and listed; invalid → error and registry unchanged; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 9: registration validation accepts valid tools and rejects invalid ones`
    - _Properties: P9_

  - [x] 11.4 Property test — name uniqueness across both maps; unknown names rejected
    - **Property 10: Tool names are unique across both maps; unknown names are rejected**
    - **Validates: Requirements 1.11, 1.12**
    - `proptest` over pre-populated registries: re-registering a held name (built-in or external) → `DuplicateTool` with the existing registration intact; a name held by neither → `tools.call` returns `UnknownTool` invoking nothing; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 10: tool names are unique across both maps`
    - _Properties: P10_

  - [x] 11.5 Property test — exactly one `tool.call` event per invocation carrying the trace id
    - **Property 5: Exactly one `tool.call` event per invocation, carrying the trace id**
    - **Validates: Requirements 1.10, 8.1, 8.2, 8.4, 8.6**
    - `proptest` over generated outcomes via fake built-ins (success, tool error, `InvalidArguments`, `Timeout`, `Unavailable`, `OutputTooLarge`), capturing emitted events; assert exactly one carrying the passed trace id and recording outcome + elapsed duration; min 100 iterations
    - Tag: `// Feature: builtin-tools, Property 5: exactly one tool.call event per invocation`
    - _Properties: P5_

- [x] 12. Checkpoint — crate builds and all nebula-tools tests pass
  - Ensure all tests pass, ask the user if questions arise.

- [x] 13. Configuration keys
  - [x] 13.1 Add `[tools.builtin]` keys to config/default.toml and nebula-config
    - Add `worktree_root` (default `"."`), `call_timeout_secs` (default 30, clamped 1..=600), `output_cap_bytes` (default 65536) to `config/default.toml` with doc comments
    - Mirror the keys in `nebula-config` with `deny_unknown_fields` and doc comments; clamp `call_timeout_secs` to the Req 1.6 range when building `BuiltinLimits`
    - Add the builtin `worktree_root` to `all_paths()` for the retired-drive doctor check
    - _Requirements: 1.6, 1.8, 7.6_

  - [x] 13.2 Unit tests — config parse/clamp/all_paths
    - Defaults parse; out-of-range `call_timeout_secs` clamps; an unknown key under `[tools.builtin]` is rejected; `worktree_root` is included in `all_paths()`
    - _Requirements: 1.6, 1.8, 7.6_

- [x] 14. Daemon wiring
  - [x] 14.1 Build `ToolContext` and call `register_all` in main.rs
    - After `ToolHost::start` (external servers loaded), build the `ToolContext` from config and call `nebula_tools::builtins::register_all(&mut host, ctx)` before wrapping the host in `Arc` (since `register_builtin` takes `&mut self`)
    - Implement the daemon-side `WorktreeRootProvider` (from config), `ResourceProvider` over `Shared::latest_snapshot()` (the same source `resources.snapshot` uses — guarantees Req 6.4), and use `DefaultClassifier` as the `CommandClassifier`
    - Pass `resources.retired_drive` (existing config, default `"C:"`) to the resolver; leave `tools.list`/`tools.call` proto and wire types unchanged (no `PROTO_VERSION` bump)
    - _Requirements: 1.3, 1.4, 6.4, 7.6_

  - [x] 14.2 Integration tests — through the daemon
    - `tools.list` returns built-ins alongside externals; `tools.call` round-trips each built-in through the real `ToolHost` and the `ToolCallOutcome` wire type; Job Object kill-on-close terminates a child process tree; `system.resources` output equals `resources.snapshot` output for the same sampler state; a telemetry event is emitted with outcome and `wall_ms` fields present
    - All paths redirected to tempdirs
    - _Requirements: 1.3, 1.4, 4.6, 6.4, 8.1, 8.3, 8.5_

- [x] 15. Documentation
  - [x] 15.1 Add crate/module docs and update ARCHITECTURE if present
    - Add module-level doc comments to `builtins`, `path`, `permit`, and `cap`; document the output-cap behavior change for external tools (reject → truncate+blob) and the injectable `#28`/`#29` boundaries
    - Update `ARCHITECTURE.md`/relevant docs with the built-in tool path if such a doc exists
    - _Requirements: 1.1, 5.7_

- [x] 16. Final gate and PR (protected set — never merge)
  - Run the full gate: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo nextest run --workspace --locked`; confirm bench `ruff` + `pytest` are unaffected
  - Open a pull request for human review and DO NOT merge it (protected-set feature)
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marked with `*` are optional test/validation sub-tasks and can be skipped for a faster MVP, but all 10 property tests and the issue #27 acceptance unit tests are strongly recommended given the security-sensitive, protected-set nature of this feature.
- Each property test uses `proptest`, runs a minimum of 100 iterations, and is tagged with its `// Feature: builtin-tools, Property N: ...` comment.
- Property placement is next to the code it verifies: P1/P2 with the path resolver (task 4), P3 with the output cap (task 5), P6 with the permission layer (task 6), P4/P7 with the shell tool (task 9), P8 with the resources tool (task 10), and P9/P10/P5 with the registration/boundary layer (task 11).
- Only coding, testing, and documentation tasks are included; no deployment, user-testing, or operational tasks.
- Checkpoints (tasks 12 and 16) provide incremental validation; task 16 runs the full gate and opens a PR without merging.

## Task Dependency Graph

```json
{
  "waves": [
    { "id": 0, "tasks": ["1", "2.1", "2.2", "4.1", "5.1", "6.1"] },
    { "id": 1, "tasks": ["2.3", "3.1", "4.2", "4.3", "5.2", "5.3", "6.2", "9.1", "10.1"] },
    { "id": 2, "tasks": ["3.2", "6.3", "7.1", "8.1", "9.2", "10.2", "10.3", "13.1"] },
    { "id": 3, "tasks": ["3.3", "7.2", "8.2", "9.3", "9.4", "9.5", "11.1", "13.2"] },
    { "id": 4, "tasks": ["3.4", "11.2", "11.3", "11.4", "11.5", "14.1"] },
    { "id": 5, "tasks": ["14.2", "15.1"] }
  ]
}
```
