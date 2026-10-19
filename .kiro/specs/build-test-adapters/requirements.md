# Requirements Document

## Introduction

This feature adds Nebula's first **external Python MCP tool server** (GitHub issue #33, design Phase 1, decision 9), providing language **build/test/lint adapters** that run a repository's build, test, and lint commands and return a **short, structured result** — per-test pass/fail with failure messages and `file:line` locations — instead of raw logs. It covers Python (uv, pytest, ruff, pyright) and Rust (cargo build, nextest, clippy).

Unlike the issue #27 built-in tools, which run **in-process in Rust inside the daemon**, this adapter is a **separate process** the daemon launches and talks to over stdio using the Model Context Protocol (MCP), per ADR-001 (Rust core, Python periphery) and ADR-002 (MCP as the universal tool boundary). There is **no Python tool template in the repository yet**; this feature **establishes** the first Python MCP tool-server scaffold, which becomes the reusable template ADR-001 refers to.

The daemon's existing tool host (merged in issues #27–#30) already owns the cross-cutting concerns: it launches a configured server in a kill-on-close Job Object (issue #30), enforces a per-call timeout and an output cap that spills over-cap output to a blob (issue #27), assigns a permission tier (issue #28), confines work to the per-task worktree (issue #29), and emits exactly one `tool.call` telemetry event. **This feature does not re-implement any of that.** It implements the Python server and its tools against the host's existing wire contract, and relies on the host for timeout, capping, tiering, confinement, and logging.

The exact wire contract the server must satisfy is fixed by the merged MCP client (`crates/nebula-tools/src/client.rs`): newline-delimited JSON-RPC 2.0 over stdin/stdout, protocol version `2025-06-18`, with `initialize` → `notifications/initialized`, `tools/list`, and `tools/call`.

This is a Phase 1, `tools`-labelled feature. It is a dependency of the executor loop (issue #32): the executor's exit criterion — "fix a failing test end to end" — requires a structured notion of whether a test is passing, which `test.run` provides.

## Glossary

- **MCP**: Model Context Protocol — the language-neutral JSON-RPC-over-stdio protocol that is Nebula's universal tool boundary (ADR-002).
- **MCP_Tool_Server**: The external process this feature introduces. It speaks MCP over its own stdin/stdout and exposes the build/test/lint tools. Launched and supervised by the daemon's `Tool_Host`.
- **Tool_Host**: The daemon-side MCP client and registry (`crates/nebula-tools`) that launches configured servers, performs the `initialize`/`tools/list` handshake, routes `tools/call`, enforces the timeout and output cap, assigns the permission tier, and logs the call. Owned by issues #27–#30; unchanged by this feature.
- **Adapter**: The per-language implementation inside the `MCP_Tool_Server` that knows how to invoke a language's build/test/lint runners and parse their output. This feature ships a **Python Adapter** (uv, pytest, ruff, pyright) and a **Rust Adapter** (cargo build, nextest, clippy).
- **test.run**: The MCP tool, named exactly `test.run`, that runs the test runner for the target language in the worktree and returns a `Structured_Result`.
- **build.run / lint.run**: The MCP tools (names subject to the Open Questions) that run the build/compile step and the lint step respectively, returning diagnostics as `Diagnostic` entries.
- **Structured_Result**: The short, bounded, machine-readable result `test.run` returns: an overall outcome (passed / failed / errored), per-test pass/fail entries, and for each failure a `Diagnostic`. It is NOT the raw log.
- **Diagnostic**: One parsed problem, carrying at least a `file`, a `line` (and optional `column`), a `severity`, a `message`, and the `source` tool that produced it (for example `pytest`, `rustc`, `nextest`, `ruff`, `clippy`, `pyright`).
- **Raw_Log**: The full, unparsed stdout/stderr of a build/test/lint run.
- **Raw_Log_Blob**: The preserved full `Raw_Log`, recoverable out of band (via the host's over-cap blob mechanism or a server-written log — see Open Questions), referenced from the `Structured_Result` rather than inlined.
- **Worktree_Root**: The confinement root the `Tool_Host` provides for the executing task (issue #29). All build/test/lint commands run with this as their working directory; nothing resolves outside it or onto the retired drive.
- **Retired_Drive**: The failing drive (`C:` by default) that must never be read from or written to (AGENTS.md hard rule 5).
- **uv**: The Python environment/dependency manager (lockfile-based, Windows-friendly) used to run Python tools, per ADR-001.
- **nextest**: `cargo nextest`, the Rust test runner used by the workspace (AGENTS.md build/test section).

## Requirements

### Requirement 1: Python MCP tool server scaffold (the first tool template)

**User Story:** As a Nebula maintainer, I want a correct Python MCP tool server that speaks the daemon's existing wire contract, so that the daemon can launch it and call its tools with no change to the host, and it becomes the template for future Python tools.

#### Acceptance Criteria

1. WHEN the Tool_Host sends an `initialize` request, THE MCP_Tool_Server SHALL reply with its capabilities and server info advertising MCP protocol version `2025-06-18`, and SHALL accept the subsequent `notifications/initialized` notification without replying to it.
2. WHEN the Tool_Host sends a `tools/list` request, THE MCP_Tool_Server SHALL reply with a `tools` array in which each entry carries a `name`, an optional `description`, and an `inputSchema` that is a valid JSON Schema object.
3. WHEN the Tool_Host sends a `tools/call` request with `{ name, arguments }`, THE MCP_Tool_Server SHALL reply with `{ content, isError }`, matching the response's JSON-RPC `id` to the request's `id`.
4. THE MCP_Tool_Server SHALL write only well-formed JSON-RPC objects, one per line, to stdout, and SHALL write all diagnostics, logs, and subprocess output destined for humans to stderr, so that stdout remains a clean protocol channel.
5. IF the Tool_Host sends a request for an unknown method or an unknown tool name, THEN THE MCP_Tool_Server SHALL reply with a JSON-RPC error (for an unknown method) or a `tools/call` result with `isError` true (for an unknown tool), and SHALL NOT terminate.
6. THE MCP_Tool_Server SHALL be a standalone `uv`-managed Python package, separate from the `bench` package, that runs under the project's Python toolchain and passes `uv run ruff check .`, `uv run ruff format --check .`, and `uv run pytest -q`.

### Requirement 2: `test.run` returns a structured per-test result

**User Story:** As the executor (issue #32), I want `test.run` to return a short structured result of a test run, so that I can decide whether a test is passing without reading raw logs.

#### Acceptance Criteria

1. WHEN `test.run` is called for a target language in the Worktree_Root, THE MCP_Tool_Server SHALL run that language's test runner with the Worktree_Root as the working directory and return a Structured_Result.
2. THE Structured_Result SHALL carry an overall outcome of exactly one of `passed`, `failed`, or `errored`.
3. WHEN a test run has one failing test, THE Structured_Result SHALL identify that failing test and SHALL carry a Diagnostic with the failure's `file`, `line`, and `message`.
4. WHEN `test.run` runs a Python (`pytest`) repository with exactly one failing test, THE Structured_Result SHALL report overall `failed` and SHALL include exactly that one failure with its `file:line` location.
5. WHEN `test.run` runs a Rust (`cargo nextest`) repository with exactly one failing test, THE Structured_Result SHALL report overall `failed` and SHALL include exactly that one failure with its `file:line` location.
6. WHEN all tests pass, THE Structured_Result SHALL report overall `passed` and SHALL carry no failure Diagnostics.

### Requirement 3: Language coverage — Python and Rust build, test, and lint

**User Story:** As a Nebula user, I want build, test, and lint adapters for both Python and Rust, so that the agent can compile, test, and lint the two languages the project uses.

#### Acceptance Criteria

1. THE Python Adapter SHALL support running tests via `uv`/`pytest`, linting via `ruff`, and type checking via `pyright`.
2. THE Rust Adapter SHALL support building via `cargo build`, testing via `cargo nextest`, and linting via `cargo clippy`.
3. WHEN a build/compile step is run, THE MCP_Tool_Server SHALL return a Structured_Result whose Diagnostics are the compiler errors and warnings with their `file:line` locations.
4. WHEN a lint step is run, THE MCP_Tool_Server SHALL return a Structured_Result whose Diagnostics are the linter findings with their `file:line` locations and severities.
5. WHERE the target language is not specified by the caller, THE MCP_Tool_Server SHALL detect the language from the Worktree_Root (for example a `Cargo.toml` for Rust, a `pyproject.toml` for Python), and IF the language cannot be determined THEN THE MCP_Tool_Server SHALL return an `isError` result naming the ambiguity rather than guessing.

### Requirement 4: Structured parsing of compiler and lint diagnostics into file:line entries

**User Story:** As the executor, I want compiler and lint output parsed into `file:line` diagnostics, so that I can act on precise locations instead of scanning logs.

#### Acceptance Criteria

1. WHEN parsing runner output, THE MCP_Tool_Server SHALL produce a Diagnostic for each distinct error/finding carrying `file`, `line`, `severity`, `message`, and `source`, and SHALL include `column` when the runner reports one.
2. THE MCP_Tool_Server SHALL parse `pytest` failures (including the failing assertion location), `cargo`/`rustc` compiler errors, `cargo nextest` test failures, `ruff` findings, `cargo clippy` findings, and `pyright` diagnostics into Diagnostics.
3. WHEN a Diagnostic's location is relative to the Worktree_Root, THE MCP_Tool_Server SHALL report `file` as a path relative to the Worktree_Root.
4. WHEN a run produces multiple diagnostics, THE MCP_Tool_Server SHALL report them in a stable order (for example by file then line) so that identical runs produce identical Structured_Results.

### Requirement 5: Bounded output with recoverable raw log

**User Story:** As a Nebula operator, I want the result returned to the model to be short while the full log is still recoverable, so that the model's context is not flooded and nothing is lost.

#### Acceptance Criteria

1. THE Structured_Result returned inline SHALL be bounded in size and SHALL NOT contain the full Raw_Log.
2. THE MCP_Tool_Server SHALL make the full Raw_Log recoverable as a Raw_Log_Blob referenced from, or alongside, the Structured_Result.
3. WHEN a run produces more diagnostics than a configured maximum, THE Structured_Result SHALL include the first diagnostics up to that maximum and SHALL record the total count, so the result stays bounded while signalling truncation.
4. THE MCP_Tool_Server SHALL exclude secret values from the Structured_Result and from anything it writes to stdout or stderr.

### Requirement 6: Confinement and subprocess hygiene

**User Story:** As the owner, I want build/test commands confined to the task's worktree and killed with the task, so that a run cannot touch the retired drive, escape the worktree, or leak processes.

#### Acceptance Criteria

1. THE MCP_Tool_Server SHALL run every build/test/lint command with the Worktree_Root as the working directory.
2. IF a requested run would operate on a path outside the Worktree_Root or on the Retired_Drive, THEN THE MCP_Tool_Server SHALL refuse it with an `isError` result and SHALL NOT run the command.
3. THE MCP_Tool_Server SHALL spawn each build/test/lint runner as an ordinary child process (not detached), so that it inherits the server's Job Object membership and is terminated when the task's job is closed (issue #30).
4. THE MCP_Tool_Server SHALL NOT write to the Retired_Drive.

### Requirement 7: Registration and wiring through the existing host

**User Story:** As a Nebula operator, I want to register the adapter server in config and have the daemon launch it, so that its tools appear and run through the existing tool host with no host changes.

#### Acceptance Criteria

1. THE System SHALL provide a `[tools.servers.<name>]` configuration entry (command, args, call_timeout_ms, max_output_bytes, env) that launches the MCP_Tool_Server, documented with an example in the project configuration.
2. WHEN the daemon starts with the adapter server configured, THE Tool_Host SHALL launch it, complete the handshake, and list its tools alongside the built-in tools, with no change to the host's protocol or code.
3. WHEN the adapter server's tools are called through the Tool_Host, THE Tool_Host SHALL apply its existing per-call timeout, output cap, permission tier, and `tool.call` telemetry to them exactly as for any other MCP server.

### Requirement 8: Robust failure handling

**User Story:** As the executor, I want missing runners, timeouts, and unparseable output to produce clean structured errors, so that a bad run never crashes the server or corrupts the protocol.

#### Acceptance Criteria

1. IF a required runner (for example `uv`, `cargo`, `nextest`, `ruff`, `clippy`, `pyright`) is not available, THEN THE MCP_Tool_Server SHALL return an `isError` Structured_Result naming the missing runner, and SHALL NOT crash.
2. IF a run's output cannot be parsed into a Structured_Result, THEN THE MCP_Tool_Server SHALL return an `isError` result with an overall outcome of `errored`, preserving the Raw_Log_Blob, rather than reporting `passed`.
3. WHILE handling any error, THE MCP_Tool_Server SHALL continue to speak the protocol correctly on stdout and SHALL NOT emit non-JSON to stdout.
4. IF the Tool_Host cancels or times out a call, THEN THE MCP_Tool_Server SHALL allow its subprocesses to be terminated via the Job Object without leaking a running process.

### Requirement 9: Fail-safe parsing (never report success on a failing run)

**User Story:** As the owner, I want a parsing miss to fail safe, so that the agent is never told a run passed when it did not.

#### Acceptance Criteria

1. IF a build/test/lint runner exits with a non-success status, THEN THE Structured_Result SHALL report overall `failed` or `errored`, never `passed`, even when no individual Diagnostic could be parsed.
2. WHEN the runner's exit status and the parsed diagnostics disagree (for example a non-zero exit with zero parsed failures), THE MCP_Tool_Server SHALL report overall `failed` or `errored` and SHALL record that diagnostics could not be fully parsed.
3. THE MCP_Tool_Server SHALL derive the overall outcome primarily from the runner's exit status, using parsed diagnostics to enrich — never to override toward success — that outcome.

## Open Questions

1. **Raw-log preservation approach.** Should the server return the Raw_Log as part of `content` and rely on the Tool_Host's over-cap blob spill (issue #27 `cap.rs`), or should the server itself persist the Raw_Log and return a reference? The host approach is simplest but makes the structured result and raw log share the inline budget; a server-written log needs a location and cleanup policy.
2. **Tool shape: one tool with a mode vs separate tools.** The issue names `test.run` specifically. Should build and lint be `build.run` / `lint.run` separate tools, or a single tool with a `mode` argument? Separate tools give clearer schemas and tiers; a single tool is fewer registrations.
3. **Server directory and package name.** Where does the package live (for example `tools/nebula-devtools/`) and what is it called? It must be a separate `uv` package from `bench/`.
4. **Language detection vs explicit argument.** Is the target language auto-detected from the Worktree_Root (Cargo.toml / pyproject.toml) only, passed as an argument only, or both (argument overrides detection)?
5. **v1 runner scope (decision 9, adapter order).** Which runners are in scope for v1? The issue lists all of uv/pytest/ruff/pyright and cargo build/nextest/clippy; design decision 9 defines an adapter order. Should pyright (which needs Node) be deferred, given it is the only runner not already a Rust/uv-native dependency?
6. **Permission tier for the adapter tools.** `test.run`/`build.run`/`lint.run` run arbitrary project build scripts (a test can execute anything). What tier should these be assigned by the classifier (issue #28) — Sandbox (confined to the worktree) or higher? This affects whether a run needs an approval.
