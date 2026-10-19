# Requirements Document

## Introduction

This feature delivers the first set of built-in tools for Nebula (a local-first AI coding agent built as a Rust workspace targeting Windows), exposed through the existing MCP tool host. It implements GitHub issue #27 ("Built-in tools: fs.\*, git.\*, shell.run, system.resources"), per design Section 6.1.

PR #42 established the `nebula-tools` crate but only the *external* half: stdio MCP servers launched from a `[tools]` manifest. There is currently no in-process built-in tool mechanism. This feature adds an **in-process built-in tool registration path** into the existing Tool Host so that Rust in-process tools appear in `tools.list` and are callable via `tools.call` alongside external tools, passing through the **same boundary**: input-schema validation, per-call timeout, output-size cap, and `tool.call` telemetry (with trace_id, args/output routed to blobs).

The built-in tools in scope are:

- **File tools** — `fs.read`, `fs.write`, `fs.list`, `fs.search`, confined to the task's worktree.
- **Git tools** — `git.status`, `git.diff`, `git.commit`, `git.branch`, confined to the worktree repository, with no push capability.
- **Shell tool** — `shell.run`, which runs through command classification and permission tiers, confined to the worktree, with a timeout, output cap, and process isolation.
- **Resource tool** — `system.resources`, which wraps the existing resource snapshot.

This is a **protected-set, security-sensitive** feature. The built-ins are part of the core/protected tool set (design 6.1): Rust, in-process, and not self-modifiable. Requirements below are deliberately precise about rejection behavior, because the primary risk is a tool reading, writing, or executing outside its permitted boundary — including onto the retired C: boot drive, which is a hard-rejected target for all access (design 7.6, AGENTS.md hard rule).

### Scope boundaries and cross-issue dependencies

- The full command classifier, permission-tier engine, and approval UX are **issue #28** ("Permission tiers and command classification"). The git-worktree-per-task lifecycle is **issue #29**. Both are planned to live in a `nebula-sandbox` crate and are **not yet built**.
- To let this feature (#27) land before #28 and #29, two external decisions are modeled as **injectable boundaries** (traits) that this feature defines, consumes, and tests against:
  1. A **Command Classifier / Approval Provider** interface that, given a parsed command, returns a tier and an approval decision. This feature ships a deterministic rules subset sufficient for its own acceptance tests and refuses commands above the no-approval threshold when approval is absent.
  2. A **Worktree Root Provider** (an injectable/configurable confinement root) that supplies the directory the tools are confined to.
- This feature does **not** build a full approval UI; it only enforces **refusal** of above-threshold commands when no approval is granted (see Requirement 4).

## Glossary

- **Nebula**: The local-first AI coding agent (Rust workspace, Windows target) that owns and runs these tools.
- **Tool_Host**: The existing `ToolHost` type in the `nebula-tools` crate. Owns registered tools, validates call arguments against each tool's input schema before the call, enforces a per-call timeout and an output-size cap, and emits `tool.call` telemetry. This feature extends it to also own in-process built-in tools.
- **Builtin_Tool**: A Rust, in-process tool registered with the Tool_Host (as opposed to an external stdio MCP server tool). Part of the core/protected set and not self-modifiable.
- **External_Tool**: A tool provided by an external stdio MCP server launched from the `[tools]` manifest (the capability delivered by PR #42).
- **Tool_Boundary**: The common processing path every tool call passes through: input-schema validation, per-call timeout, output-size cap, and `tool.call` telemetry emission with trace_id and args/output blob routing.
- **Worktree_Root**: The single configured directory to which file and shell operations are confined for a task. Supplied by the Worktree_Root_Provider. Full worktree-per-task lifecycle management is issue #29.
- **Worktree_Root_Provider**: The injectable interface that supplies the current Worktree_Root to the built-in tools.
- **Retired_Drive**: The drive designated by the `resources.retired_drive` configuration value (default `"C:"`). It is the failing boot drive and is rejected outright for all access, including reads (design 7.6, AGENTS.md hard rule).
- **Path_Resolver**: The single path-resolution and confinement routine used by all file and shell path checks. Fully resolves a candidate path (expanding `..`, following symlinks and NTFS junctions, canonicalizing absolute paths) and determines whether the resolved path lies inside the Worktree_Root and off the Retired_Drive.
- **Command_Classifier**: The injectable interface that parses a shell command, maps it to a permission tier, and reports the approval decision. The full engine is issue #28; this feature defines the interface and ships a deterministic rules subset for its acceptance tests.
- **Permission_Tier**: A classification of an operation's privilege level (design 7.1). Tier 0 (Read) and Tier 1 (Sandbox write inside the worktree) are never approved (i.e., require no approval). Tier 2 (Workspace) requires per-policy approval. Tier 3 (System) always requires approval.
- **No_Approval_Threshold**: The highest Permission_Tier that executes without an approval decision (Tier 1). Any command classified above this threshold (Tier 2 or Tier 3) must be refused unless an approval decision is present.
- **Approval_Decision**: A grant, supplied through the Command_Classifier interface, that authorizes execution of a command classified above the No_Approval_Threshold.
- **Job_Object**: A Windows Job Object configured with kill-on-close and resource/process-count limits, used to isolate child processes (design 7.3).
- **Scrubbed_Environment**: A child-process environment with secrets removed and a controlled PATH, working directory pinned to the Worktree_Root (design 7.3).
- **Output_Cap**: The maximum size of tool output returned inline. Output exceeding the cap is truncated with a visible marker and the full output is written to a blob.
- **Blob_Store**: The content-addressed store in the `nebula-telemetry` crate used for large payloads. `tool.call` already routes args/output to it.
- **Resource_Snapshot**: The `nebula_proto::ResourceSnapshot` value produced by the existing Sampler in `nebula-resources` and already exposed by the daemon's `resources.snapshot` method.
- **Trace_Id**: The correlation identifier inherited from the Tool_Boundary and recorded on every `tool.call` telemetry event.
- **Tool_Error**: The existing `ToolError` enum in `nebula-tools` with variants `UnknownServer`, `UnknownTool`, `InvalidArguments`, `Launch`, `Unavailable`, `Timeout`, `OutputTooLarge`, and `Protocol`.

## Requirements

### Requirement 1: In-process built-in tool mechanism

**User Story:** As a Nebula developer, I want to register Rust in-process tools with the Tool Host, so that built-in tools are discoverable and callable through the same validated boundary as external tools.

#### Acceptance Criteria

1. THE Tool_Host SHALL provide a registration path that accepts a Builtin_Tool together with a tool name of 1 to 128 characters and a non-empty input schema, and SHALL make the registered Builtin_Tool resolvable by that exact tool name.
2. IF a Builtin_Tool registration supplies a tool name that is empty, longer than 128 characters, or omits an input schema, THEN THE Tool_Host SHALL reject the registration, return a descriptive error, and SHALL NOT register the tool.
3. WHEN a client invokes `tools.list`, THE Tool_Host SHALL return a list that includes every currently registered Builtin_Tool and every registered External_Tool, each entry carrying its tool name and input schema.
4. WHEN a client invokes `tools.call` targeting a registered Builtin_Tool, THE Tool_Host SHALL validate the call arguments against that tool's input schema before invoking the tool.
5. IF the call arguments for a Builtin_Tool fail input-schema validation, THEN THE Tool_Host SHALL reject the call with Tool_Error `InvalidArguments` identifying the failing tool, and SHALL NOT invoke the tool.
6. WHEN a registered Builtin_Tool is invoked, THE Tool_Host SHALL enforce the per-call timeout (default 30 seconds, configurable from 1 second to 600 seconds) on the tool's execution.
7. IF a Builtin_Tool invocation does not complete before the per-call timeout elapses, THEN THE Tool_Host SHALL terminate the invocation and return Tool_Error `Timeout` recording the elapsed timeout in milliseconds.
8. WHEN a registered Builtin_Tool returns output within the Output_Cap (default 65,536 bytes), THE Tool_Host SHALL return the complete output inline.
9. WHEN a registered Builtin_Tool returns output exceeding the Output_Cap, THE Tool_Host SHALL apply the Output_Cap handling defined in Requirement 5 (inline truncation with a visible marker plus full output routed to the Blob_Store).
10. WHEN a registered Builtin_Tool invocation completes, whether it succeeds or returns a Tool_Error, THE Tool_Host SHALL emit exactly one `tool.call` telemetry event for that invocation through the Tool_Boundary.
11. IF a client invokes `tools.call` with a tool name that matches no registered Builtin_Tool and no registered External_Tool, THEN THE Tool_Host SHALL return Tool_Error `UnknownTool` identifying the requested name, and SHALL NOT invoke any tool.
12. IF a Builtin_Tool registration requests a tool name already held by a registered Builtin_Tool or External_Tool, OR an External_Tool registration requests a tool name already held by a registered Builtin_Tool, THEN THE Tool_Host SHALL reject the registration, return a descriptive error identifying the conflicting name, and SHALL NOT replace or shadow the existing registration.

### Requirement 2: File tools confined to the worktree

**User Story:** As a Nebula user, I want file read, write, list, and search tools that operate only inside the task's worktree, so that the agent cannot read or modify files outside its sandbox or on the failing boot drive.

#### Acceptance Criteria

1. THE Tool_Host SHALL register the Builtin_Tools `fs.read`, `fs.write`, `fs.list`, and `fs.search`.
2. WHEN `fs.read`, `fs.list`, or `fs.search` receives a path argument, THE Builtin_Tool SHALL resolve that path through the Path_Resolver before performing any file operation, and WHEN `fs.write` receives a path argument for a new file, THE Builtin_Tool SHALL resolve the parent directory of that path through the Path_Resolver before performing any file operation.
3. WHEN the Path_Resolver reports the resolved path as permitted (inside the Worktree_Root and off the Retired_Drive), THE `fs.read` Builtin_Tool SHALL return the contents of the target file.
4. WHEN the Path_Resolver reports the resolved path as permitted, THE `fs.list` Builtin_Tool SHALL return the directory entries of the target directory.
5. WHEN the Path_Resolver reports the resolved path as permitted, THE `fs.search` Builtin_Tool SHALL return the matches found within the permitted path.
6. WHEN the Path_Resolver reports the resolved target path (or, for a new file, its parent directory) as permitted, THE `fs.write` Builtin_Tool SHALL create the file if it does not exist or update it if it exists.
7. IF the Path_Resolver reports a path argument as rejected for lying outside the Worktree_Root (including rejection due to `..` segments, an absolute path resolving outside the Worktree_Root, or a symbolic link or NTFS junction whose target lies outside the Worktree_Root), THEN THE Builtin_Tool SHALL reject the call with Tool_Error `InvalidArguments`, SHALL NOT perform the file operation, and SHALL leave the filesystem unchanged.
8. IF the Path_Resolver reports a path argument as rejected for lying on the Retired_Drive (including when the operation is a read and including a symbolic link or NTFS junction whose target lies on the Retired_Drive), THEN THE Builtin_Tool SHALL reject the call with Tool_Error `InvalidArguments`, SHALL NOT perform the file operation, and SHALL leave the filesystem unchanged.
9. IF a resolved path is both outside the Worktree_Root and on the Retired_Drive, THEN THE Builtin_Tool SHALL reject the call with Tool_Error `InvalidArguments` on the retired-drive basis, taking precedence over the worktree-escape basis.
10. THE `fs.read`, `fs.list`, and `fs.search` Builtin_Tools SHALL be classified at Permission_Tier 0 (Read) at registration time.
11. THE `fs.write` Builtin_Tool SHALL be classified at Permission_Tier 1 (Sandbox write) at registration time, independent of the validity of any specific path argument.
12. WHEN `fs.read` output or `fs.search` output exceeds the Output_Cap, THE Builtin_Tool SHALL apply the Output_Cap handling defined in Requirement 5.

### Requirement 3: Git tools confined to the worktree repository

**User Story:** As a Nebula user, I want git status, diff, commit, and branch tools scoped to the worktree repository with no push capability, so that the agent can manage local history while pushing stays with the bot PR flow.

#### Acceptance Criteria

1. THE Tool_Host SHALL register the Builtin_Tools `git.status`, `git.diff`, `git.commit`, and `git.branch`.
2. WHEN a git Builtin_Tool is invoked, THE Builtin_Tool SHALL operate only on the git repository whose root is the Worktree_Root and SHALL NOT operate on any repository outside the Worktree_Root.
3. IF the Worktree_Root does not contain a git repository, THEN THE invoked git Builtin_Tool SHALL return Tool_Error `Unavailable` and SHALL NOT modify any repository state.
4. THE git Builtin_Tools SHALL NOT expose any push operation and SHALL NOT perform any operation that transmits commits or refs to a remote.
5. IF a git Builtin_Tool invocation requests a push or any other remote-mutating operation, THEN THE Builtin_Tool SHALL refuse the request, SHALL NOT contact any remote, and SHALL return Tool_Error `InvalidArguments`.
6. WHEN `git.status` is invoked, THE Builtin_Tool SHALL return the working-tree status for the Worktree_Root repository.
7. WHEN `git.diff` is invoked, THE Builtin_Tool SHALL return the diff for the Worktree_Root repository.
8. WHEN `git.commit` is invoked with a non-empty commit message, THE Builtin_Tool SHALL create a commit of the staged changes in the Worktree_Root repository.
9. IF `git.commit` is invoked with a missing or empty commit message, THEN THE Builtin_Tool SHALL reject the call with Tool_Error `InvalidArguments` and SHALL NOT create a commit.
10. IF `git.commit` is invoked when the Worktree_Root repository has no staged changes, THEN THE Builtin_Tool SHALL return Tool_Error `Unavailable` and SHALL NOT create a commit.
11. WHEN `git.branch` is invoked to list branches, THE Builtin_Tool SHALL return the branches of the Worktree_Root repository, and WHEN `git.branch` is invoked to create a branch with a valid branch name that does not already exist, THE Builtin_Tool SHALL create that branch in the Worktree_Root repository.
12. IF `git.branch` is invoked to create a branch whose name is invalid or already exists in the Worktree_Root repository, THEN THE Builtin_Tool SHALL reject the call with Tool_Error `InvalidArguments` and SHALL NOT create or modify any branch.
13. IF the underlying git invocation for a git Builtin_Tool fails for a reason other than a missing repository, THEN THE Builtin_Tool SHALL return Tool_Error `Unavailable` and SHALL NOT leave the repository in a partially modified state.
14. THE `git.status` and `git.diff` Builtin_Tools SHALL be classified at Permission_Tier 0 (Read).
15. THE `git.commit` and `git.branch` Builtin_Tools SHALL be classified at Permission_Tier 1 (Sandbox write) when operating on the Worktree_Root repository.
16. WHEN `git.diff` output exceeds the Output_Cap, THE Builtin_Tool SHALL apply the Output_Cap handling defined in Requirement 5.

### Requirement 4: Shell tool with classification, confinement, and refusal

**User Story:** As a Nebula user, I want a shell tool that classifies and confines every command and refuses privileged commands without approval, so that the agent cannot run dangerous or out-of-sandbox commands on my machine.

#### Acceptance Criteria

1. WHEN the Tool_Host initializes, THE Tool_Host SHALL register exactly one Builtin_Tool named `shell.run` such that the tool is discoverable by name in the Tool_Host registry.
2. WHEN `shell.run` receives a command invocation, THE `shell.run` Builtin_Tool SHALL submit the command to the injected Command_Classifier and obtain both a Permission_Tier (one of Tier 0 Read, Tier 1 Sandbox-write, Tier 2 Workspace, Tier 3 System) and an Approval_Decision before starting any child process.
3. IF the Command_Classifier returns a Permission_Tier less than or equal to the No_Approval_Threshold (Tier 1), THEN THE `shell.run` Builtin_Tool SHALL execute the command without requiring an Approval_Decision.
4. IF the Command_Classifier returns a Permission_Tier strictly greater than the No_Approval_Threshold (Tier 1) and no Approval_Decision is present, THEN THE `shell.run` Builtin_Tool SHALL refuse the command, SHALL NOT start any child process, and SHALL return Tool_Error `InvalidArguments` with an error indication that the command requires approval.
5. WHEN the Command_Classifier returns a Permission_Tier strictly greater than the No_Approval_Threshold (Tier 1) together with a present Approval_Decision, THE `shell.run` Builtin_Tool SHALL execute the command.
6. WHEN `shell.run` starts a child process, THE `shell.run` Builtin_Tool SHALL assign the child process to a Windows Job_Object configured with kill-on-close such that closing the Job_Object handle terminates the child process and all its descendants.
7. WHEN `shell.run` starts a child process, THE `shell.run` Builtin_Tool SHALL launch the child process with a Scrubbed_Environment in which secret-bearing environment variables are removed and PATH is set to the controlled value, and with its working directory set to the Worktree_Root.
8. WHEN `shell.run` starts a child process, THE `shell.run` Builtin_Tool SHALL enforce the per-call timeout by measuring elapsed wall-clock time from child process start.
9. IF a `shell.run` child process has not exited when its elapsed wall-clock time reaches the per-call timeout, THEN THE `shell.run` Builtin_Tool SHALL terminate the child process and all its descendants via the Job_Object and SHALL return Tool_Error `Timeout`.
10. WHEN the cumulative output produced by a `shell.run` child process exceeds the Output_Cap, THE `shell.run` Builtin_Tool SHALL apply the Output_Cap handling defined in Requirement 5.
11. THE `shell.run` Builtin_Tool SHALL obtain the Permission_Tier and Approval_Decision for a command solely through the Command_Classifier interface.
12. WHERE a model second opinion is available, THE `shell.run` Builtin_Tool SHALL treat the second opinion as advisory only and SHALL NOT use it to assign a Permission_Tier lower than the Permission_Tier returned by the Command_Classifier.

### Requirement 5: Output cap truncation and blob storage

**User Story:** As a Nebula operator, I want oversized tool output truncated with a marker and the full output preserved in a blob, so that inline responses stay bounded while nothing is lost.

#### Acceptance Criteria

1. THE Tool_Host SHALL measure a Builtin_Tool's output size in bytes against the Output_Cap.
2. WHEN a Builtin_Tool produces output whose size is less than or equal to the Output_Cap, THE Tool_Host SHALL return the complete output inline with no blob stored or referenced.
3. WHEN a Builtin_Tool produces output whose size is strictly greater than the Output_Cap, THE Tool_Host SHALL store the full, untruncated output in the Blob_Store before returning any inline output.
4. WHEN a Builtin_Tool produces output whose size is strictly greater than the Output_Cap, THE Tool_Host SHALL return output truncated at a UTF-8 character boundary such that the truncated prefix size is within the Output_Cap, followed by a fixed truncation marker that signals truncation occurred and that the full output is available in the Blob_Store.
5. WHEN output exceeding the Output_Cap has been stored in the Blob_Store, THE Tool_Host SHALL record a reference to the stored blob on the corresponding `tool.call` telemetry event for the same invocation.
6. IF the Blob_Store write fails while storing output that exceeds the Output_Cap, THEN THE Tool_Host SHALL return Tool_Error `OutputTooLarge` carrying the truncated prefix (truncated at a UTF-8 character boundary within the Output_Cap) for diagnostic purposes, SHALL NOT record a blob reference, and SHALL NOT return the truncated output as a successful result.
7. THE Output_Cap handling defined in this requirement SHALL apply uniformly to `shell.run` output, `fs.read` output, and `fs.search` output.

### Requirement 6: System resources tool

**User Story:** As a Nebula user, I want a resource tool that reports current system resources, so that the agent can observe host load through the same tool interface.

#### Acceptance Criteria

1. THE Tool_Host SHALL register the Builtin_Tool `system.resources`.
2. WHEN `system.resources` is invoked and the resource Sampler has produced at least one Resource_Snapshot, THE Builtin_Tool SHALL return the most recently produced Resource_Snapshot from the Sampler.
3. IF `system.resources` is invoked and the resource Sampler has not yet produced any Resource_Snapshot, THEN THE Builtin_Tool SHALL return Tool_Error `Unavailable` and SHALL NOT return a Resource_Snapshot.
4. WHEN `system.resources` returns a Resource_Snapshot, THE Builtin_Tool SHALL return a Resource_Snapshot whose fields are equal, field for field, to the Resource_Snapshot that the daemon's `resources.snapshot` method returns for the same Sampler state.
5. THE `system.resources` Builtin_Tool SHALL be classified at Permission_Tier 0 (Read).

### Requirement 7: Shared path-safety routine

**User Story:** As a Nebula developer, I want one path-resolution and confinement routine used by every file and shell path check, so that path-escape protection is consistent and testable in one place.

#### Acceptance Criteria

1. THE Path_Resolver SHALL be the single routine invoked by every file Builtin_Tool and by `shell.run` path-argument checks to resolve and confine candidate paths, such that no file Builtin_Tool or `shell.run` path check performs confinement evaluation through any other routine.
2. WHEN the Path_Resolver receives a candidate path, THE Path_Resolver SHALL produce a fully resolved path by expanding all `..` and `.` segments, canonicalizing the absolute form, and following all symbolic links and NTFS junctions, before evaluating confinement.
3. IF a candidate path does not exist and therefore cannot be canonicalized directly, THEN THE Path_Resolver SHALL resolve its nearest existing ancestor directory and compose the resolved path from that canonicalized ancestor and the remaining non-existent segments, before evaluating confinement.
4. WHEN a resolved path is lexically contained within the Worktree_Root and its drive letter does not case-insensitively equal the Retired_Drive (default `C:`), THE Path_Resolver SHALL report the path as permitted.
5. IF a resolved path is not lexically contained within the Worktree_Root, THEN THE Path_Resolver SHALL report the path as rejected with a reason value that identifies the rejection as worktree-escape and is distinct from the retired-drive reason value.
6. IF a resolved path has a drive letter that case-insensitively equals the Retired_Drive, THEN THE Path_Resolver SHALL report the path as rejected with a reason value that identifies the rejection as retired-drive, including when the requested operation is a read.
7. WHEN a Builtin_Tool receives a rejection result from the Path_Resolver, THE Builtin_Tool SHALL return Tool_Error `InvalidArguments` and SHALL NOT perform the requested file or shell operation.

### Requirement 8: Security logging for every built-in tool call

**User Story:** As a Nebula operator, I want every built-in tool call logged with correlation and timing and with secrets scrubbed, so that tool activity is auditable and no tool can bypass logging.

#### Acceptance Criteria

1. WHEN any Builtin_Tool is invoked through the Tool_Host, THE Tool_Host SHALL emit a `tool.call` telemetry event for that invocation.
2. THE `tool.call` telemetry event SHALL include the Trace_Id inherited from the Tool_Boundary.
3. THE `tool.call` telemetry event SHALL record the invocation outcome as a success-or-failure classification and the invocation timing as an elapsed duration.
4. IF a Builtin_Tool invocation ends in a Tool_Error, a timeout, or an input-validation rejection, THEN THE Tool_Host SHALL still emit a `tool.call` telemetry event recording that outcome.
5. WHEN a `tool.call` telemetry event records arguments or output, whether inline or routed to the Blob_Store, THE Tool_Host SHALL scrub known secret values and secret-pattern matches from the recorded content.
6. THE Tool_Host SHALL route every Builtin_Tool invocation through the Tool_Boundary such that no Builtin_Tool invocation path produces a result without emitting a `tool.call` telemetry event.

## Assumptions, Constraints, and Dependencies

### Dependencies

- **Issue #28 (Permission tiers and command classification)**: The full command classifier, permission-tier engine, and approval UX are out of scope here. This feature defines and consumes the **Command_Classifier** interface and ships a deterministic rules subset sufficient for its own acceptance tests (notably refusing a Tier 3 command when no Approval_Decision is present). The planned home for the full engine is the `nebula-sandbox` crate.
- **Issue #29 (Git worktree per task)**: The full worktree-per-task lifecycle is out of scope here. This feature defines and consumes the **Worktree_Root_Provider** interface, and the confinement root is injectable/configurable so this feature can land before #29.

### Constraints

- The built-in tools are part of the core/protected set (design 6.1): Rust, in-process, and not self-modifiable.
- All built-in tool calls must pass through the existing Tool_Boundary (schema validation, timeout, output cap, `tool.call` telemetry). No built-in tool may bypass this boundary.
- The Retired_Drive (default `C:`, from `resources.retired_drive`) is rejected for all access including reads (design 7.6, AGENTS.md hard rule).
- `git.*` tools must not push; pushing is handled by the bot PR flow and is Tier 3 (design 7.6).
- `shell.run` child processes run inside a Job_Object with a Scrubbed_Environment, a controlled PATH, and the working directory pinned to the Worktree_Root (design 7.3).
- A model second opinion is advisory only and can never lower a Permission_Tier (design 7.2).

### Assumptions

- The `nebula-telemetry` Blob_Store, the `nebula-resources` Sampler, and the daemon `resources.snapshot` and `tools.list`/`tools.call` methods already exist and are reused rather than reimplemented.
- The No_Approval_Threshold for this feature is Permission_Tier 1; commands classified at Tier 2 or Tier 3 require an Approval_Decision.
- A full approval UI is out of scope; this feature only enforces refusal of above-threshold commands when no Approval_Decision is granted.

### Testability notes (mapping to issue #27 acceptance criteria)

- **"Each tool has unit tests, including path-escape attempts (.., symlinks or junctions, absolute paths, C:)"** — covered by Requirement 2 (criteria 7–9), Requirement 7 (criteria 2–6), and the per-tool registration criteria in Requirements 2, 3, 4, and 6.
- **"shell.run refuses tier-3 commands without approval"** — covered by Requirement 4 (criteria 4 and 11–12).
- **"Tool outputs larger than the cap are truncated with a marker, and the full output goes to a blob"** — covered by Requirement 5 (criteria 3–6).
