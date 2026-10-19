# Implementation Plan: Build/Test Adapters (nebula-devtools, the first Python MCP tool server)

## Overview

This plan builds `nebula-devtools` (GitHub issue #33, design Phase 1 decision 9): Nebula's first external Python MCP tool server, a `uv`-managed package under `tools/nebula-devtools/` that the daemon launches over stdio and that exposes `test.run`, `build.run`, and `lint.run` for Python (uv/pytest/ruff) and Rust (cargo build/nextest/clippy). Each tool runs a runner inside the task's worktree and returns a short, structured result â€” overall outcome plus `file:line` diagnostics â€” while the raw log rides along as a second content block that the host spills to a blob when over the cap.

The daemon's tool host (issues #27â€“#30) already owns launch-in-a-Job-Object, the per-call timeout, the output cap + blob spill, the permission tier, worktree confinement, and the `tool.call` event. This plan implements only the Python server and its adapters against the host's existing wire contract (`crates/nebula-tools/src/client.rs`: newline-delimited JSON-RPC 2.0, protocol `2025-06-18`, `initialize`/`notifications/initialized`, `tools/list`, `tools/call`). The only Rust-side change is a documented config example and optional Sandbox-tier classifier rules.

Decisions baked in (from requirements review): raw log via the host's over-cap blob spill (no server-written log); three separate tools; v1 runners are uv/pytest + ruff and cargo build/nextest/clippy (**pyright deferred** â€” it needs Node); Sandbox tier; package at `tools/nebula-devtools/`; explicit `language` arg wins else auto-detect.

Conventions: Python â‰¥3.12, `uv`-managed, `ruff` (line-length 100; select E/F/I/UP/B/SIM) and `pytest`, matching `bench/pyproject.toml`. The package passes `uv run ruff check . && uv run ruff format --check . && uv run pytest -q`. Outcome is derived from the runner's exit status, never from the parse (fail-safe). stdout is protocol-only; all logs/diagnostics go to stderr.

## Tasks

- [x] 1. Scaffold the `nebula-devtools` uv package
  - [x] 1.1 Create the package skeleton and tooling config
    - Create `tools/nebula-devtools/pyproject.toml` (name `nebula-devtools`, `requires-python = ">=3.12"`, dev deps `pytest`, `ruff`; `[project.scripts] nebula-devtools = "nebula_devtools.__main__:main"`; `[tool.ruff]` line-length 100 and the same lint `select` as bench; hatchling build targeting the `nebula_devtools` package)
    - Create `tools/nebula-devtools/nebula_devtools/__init__.py` and `tools/nebula-devtools/.gitignore` (ignore `.venv`, `__pycache__`, `.pytest_cache`, `.ruff_cache`)
    - Create `tools/nebula-devtools/README.md` noting this is the first Python MCP tool server / reusable template, how it is launched (`[tools.servers.devtools]`), and that stdout is protocol-only
    - _Requirements: 1.6_

- [x] 2. The data model and subprocess runner
  - [x] 2.1 Implement `model.py` â€” Outcome, Severity, Diagnostic, StructuredResult
    - Create `nebula_devtools/model.py` with the `Outcome`/`Severity` enums, frozen `Diagnostic` (file/line/message/source/severity/optional column), and `StructuredResult` (outcome/source/language/diagnostics/total_diagnostics/truncated/tests_passed/tests_failed/note)
    - Implement `StructuredResult.to_json()` emitting a compact dict; sort diagnostics by `(file, line, column)`; cap the diagnostics list at `MAX_DIAGNOSTICS` (50) setting `total_diagnostics` and `truncated`
    - _Requirements: 4.1, 4.4, 5.3_
    - _Properties: P2, P3_
  - [x]* 2.2 Unit tests for the model
    - Create `tests/test_model.py`: diagnostics sort stably; the list caps at MAX_DIAGNOSTICS with `total_diagnostics`/`truncated` set; `to_json` round-trips the fields
    - _Requirements: 4.4, 5.3_
  - [x] 2.3 Implement `run.py` â€” the worktree subprocess runner
    - Create `nebula_devtools/run.py` with `RunOutput` and `run_command(argv, cwd, env=None)`: spawn an ordinary child (no detach / no new process group) with `cwd` as working directory, capture stdout/stderr/exit code; a missing runner returns `RunOutput(exit_code=None, spawn_error=...)` rather than raising; impose no internal timeout (the host owns it)
    - _Requirements: 6.1, 6.3, 8.1_

- [x] 3. Language detection and confinement
  - [x] 3.1 Implement `detect.py` and the confinement check
    - Create `nebula_devtools/detect.py`: `detect_language(cwd)` â†’ `"rust"` if `Cargo.toml` present, `"python"` if `pyproject.toml` present, else `None`; and `resolve_language(arg, cwd)` where an explicit arg wins, else detection, else an "ambiguous/undetermined" error
    - Add a `within_worktree(path, worktree_root, retired_drive)` guard: resolve to absolute, reject paths outside the worktree root or whose drive equals the retired drive (default `C:`, overridable via `NEBULA_RETIRED_DRIVE`)
    - _Requirements: 3.5, 6.1, 6.2, 6.4_
    - _Properties: P5_
  - [x]* 3.2 Unit tests for detection and confinement
    - Create `tests/test_detect.py`: Cargo.tomlâ†’rust, pyproject.tomlâ†’python, neitherâ†’None/error, explicit arg overrides; a path outside the worktree and a `C:` path are rejected
    - _Requirements: 3.5, 6.2_

- [x] 4. Parsers (fixture-driven)
  - [x] 4.1 Capture runner-output fixtures
    - Create `tests/fixtures/` with real captured outputs: a failing `pytest` run, a failing `cargo nextest` run, a `cargo build` with a compile error, a `cargo clippy` finding, and a `ruff` JSON finding (small, representative, secret-free)
    - _Requirements: 4.2_
  - [x] 4.2 Implement `cargo_parser.py` (build + clippy, JSON message format)
    - Parse `--message-format json` lines; map `compiler-message` spans to `Diagnostic` (file relative to worktree, line, column, severity, message, source `rustc`/`clippy`)
    - _Requirements: 4.1, 4.2, 4.3_
  - [x] 4.3 Implement `ruff_parser.py` (JSON output)
    - Parse `ruff --output-format json` array into `Diagnostic`s (filename, row/column, code+message, source `ruff`, severity warning)
    - _Requirements: 4.1, 4.2, 4.3_
  - [x] 4.4 Implement `pytest_parser.py`
    - Parse pytest failure reports into one `Diagnostic` per failing test with its assertion `file:line` and message (choose the lightest reliable mode â€” short traceback parse or a JSON report already available via uv)
    - _Requirements: 2.3, 4.1, 4.3_
  - [x] 4.5 Implement `nextest_parser.py`
    - Derive pass/fail from exit status; parse the failure summary and recover each failure's `file:line` from the captured panic/assert location; one `Diagnostic` per failing test, source `nextest`
    - _Requirements: 2.3, 2.5, 4.1, 4.3_
  - [x]* 4.6 Parser unit tests against the fixtures
    - Create `tests/test_parsers.py`: each fixture â†’ exact expected `Diagnostic[]`, stable order; a non-zero exit with unparseable output yields no false "passed"
    - _Requirements: 4.1, 4.2, 4.3, 4.4, 9.2_

- [x] 5. Adapters
  - [x] 5.1 Implement `adapters/python_adapter.py`
    - `test` â†’ `uv run pytest` (machine-friendly mode) via `run.py`, parsed by `pytest_parser`; `lint` â†’ `uv run ruff check --output-format json .`, parsed by `ruff_parser`; `build` â†’ no-op `passed` with a note (Python has no separate build)
    - Build the `StructuredResult` with exit-status-first outcome (Requirement 9); return `(result, RunOutput)`
    - _Requirements: 3.1, 9.1, 9.3_
  - [x] 5.2 Implement `adapters/rust_adapter.py`
    - `build` â†’ `cargo build --message-format json`; `test` â†’ `cargo nextest run`; `lint` â†’ `cargo clippy --message-format json`, parsed by the matching parsers; exit-status-first outcome
    - _Requirements: 3.2, 3.3, 3.4, 9.1, 9.3_
  - [x]* 5.3 Adapter end-to-end tests (gated on toolchain)
    - Create `tests/test_adapters_e2e.py` with a tiny repo that has one failing pytest test and one with a failing nextest test; assert overall `failed` and exactly that one failure with its `file:line`; skip cleanly if `uv`/`cargo` is absent, and assert the missing-runner `isError` path when so
    - _Requirements: 2.4, 2.5, 8.1_

- [x] 6. The MCP protocol loop and server
  - [x] 6.1 Implement `mcp.py` â€” JSON-RPC 2.0 over stdin/stdout
    - Read stdin line by line, parse each as a JSON object, dispatch by method; a message with an `id` always gets a matching-id reply, a notification (no id) never does; configure logging to stderr; never write non-JSON to stdout
    - Handle `initialize` (reply protocolVersion `2025-06-18`, capabilities, serverInfo), `notifications/initialized` (no reply), `tools/list`, `tools/call`; unknown method â†’ JSON-RPC error -32601
    - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5_
    - _Properties: P4_
  - [x] 6.2 Implement `server.py` â€” the tool registry and dispatch
    - Advertise `test.run`, `build.run`, `lint.run` in `tools/list`, each with a JSON Schema `inputSchema` (optional `language`, optional `cwd`); on `tools/call` resolve language (arg wins else detect), run the confinement guard, dispatch to the adapter verb, and build the `{ content: [structured, raw_log], isError }` response (`isError` = outcome != passed); unknown tool name â†’ `isError` result
    - _Requirements: 1.3, 1.5, 2.1, 2.2, 5.1, 5.2, 6.1, 6.2, 8.2_
    - _Properties: P1, P4_
  - [x] 6.3 Implement `__main__.py` â€” the entry point
    - `main()` runs the stdio server loop; a top-level guard turns any unexpected exception into a logged-to-stderr message without emitting non-JSON on stdout or exiting mid-request
    - _Requirements: 1.4, 8.3_
  - [x]* 6.4 MCP loop tests
    - Create `tests/test_mcp.py`: handshake (initializeâ†’initialized), `tools/list` lists the three tools with schemas, `tools/call` returns `{content,isError}`, unknown method â†’ -32601, unknown tool â†’ `isError`, and a property test that every stdout line parses as JSON (P4)
    - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5_
    - _Properties: P4_

- [x] 7. Checkpoint â€” the Python package is self-consistent and green
  - Run `uv run ruff check .`, `uv run ruff format --check .`, and `uv run pytest -q` in `tools/nebula-devtools/`; all pass. Confirm the fixture-driven parser tests and the MCP loop tests pass without any toolchain beyond the dev deps.
  - Ensure all tests pass; ask the user if questions arise.

- [x] 8. Rust-side wiring (config example and optional tier rules)
  - [x] 8.1 Add a documented `[tools.servers.devtools]` example to config
    - Add a commented example entry to `config/default.toml` (command `uv`, args launching `nebula-devtools`, `call_timeout_ms`, `max_output_bytes`, `env` with `NEBULA_RETIRED_DRIVE`), explaining it is enabled in the local override; confirm `nebula-config` still parses the default config (its existing tests cover this)
    - _Requirements: 7.1, 7.2, 7.3_
  - [x] 8.2 Record the Sandbox-tier decision for the adapter tools (no rules.toml change)
    - **Architecture correction:** the `nebula-sandbox` classifier (`rules.toml` + `RulesClassifier`) matches on *shell command base names* (`rm`, `git`, `cargo`) via `classify(command, args)`. External MCP tools (`test.run`/`build.run`/`lint.run`) are dispatched to the server by name and never pass through the classifier, so a `rules.toml` row for them would be dead, never-matched data in a protected-set file (AGENTS.md hard rule 9). Per-external-tool tier assignment does not exist in the merged #27â€“#30 code; it is the executor's responsibility (#32), which is not yet built.
    - Therefore add **no** `rules.toml` rows. Instead, document the Sandbox-tier intent (decision 4) in the design and in `tools/nebula-devtools/README.md` so #32 wires `test.run`/`build.run`/`lint.run` at Sandbox when it adds external-tool tiering. The config example (`call_timeout_ms`, worktree confinement via the host, Job Object via #30) already bounds the tools.
    - _Requirements: 7.3_
  - [x]* 8.3 Confirm the Rust gate is unaffected
    - `cargo nextest run -p nebula-config -p nebula-sandbox --offline` passes with the config example (no rules.toml change)
    - _Requirements: 7.2_

- [x] 9. Documentation and final gate (open PR, do not merge)
  - Update `docs/ARCHITECTURE.md` (and the docs index) with the `tools/nebula-devtools` entry: the first external Python MCP tool server, its three tools, where it lives, and how it is enabled. Note pyright is deferred.
  - Run the full gate: in `tools/nebula-devtools/` `uv run ruff check . && uv run ruff format --check . && uv run pytest -q`; and for the Rust change `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo nextest run --workspace --locked` (limited build parallelism to respect the machine's commit budget). Confirm the existing bench harness is unaffected.
  - Open a pull request on a `feat/â€¦` branch for review and DO NOT merge it (AGENTS.md hard rule 1); this feature touches no protected-set code, but the merge decision is the owner's.
  - Ensure all tests pass; ask the user if questions arise.

## Notes

- Tasks marked `*` are optional test sub-tasks; given the issue's acceptance criteria hinge on parsing correctness, the parser and e2e tests (4.6, 5.3, 6.4) are strongly recommended.
- The headline acceptance (one failing pytest test and one failing nextest test each return exactly that failure with its location) is proven by task 5.3; compiler/lint `file:line` parsing by tasks 4.2â€“4.5 and 4.6; output capping + raw-log-to-blob by the host plus tasks 6.2 and 2.1.
- No change to the Rust tool-host protocol or code is required beyond the config example and the optional classifier rules; the server conforms to the existing contract.

## Task Dependency Graph

```json
{
  "waves": [
    { "id": 0, "tasks": ["1.1"] },
    { "id": 1, "tasks": ["2.1", "2.3", "3.1", "4.1"] },
    { "id": 2, "tasks": ["2.2", "3.2", "4.2", "4.3", "4.4", "4.5"] },
    { "id": 3, "tasks": ["4.6", "5.1", "5.2"] },
    { "id": 4, "tasks": ["5.3", "6.1"] },
    { "id": 5, "tasks": ["6.2", "6.3"] },
    { "id": 6, "tasks": ["6.4"] },
    { "id": 7, "tasks": ["8.1", "8.2"] },
    { "id": 8, "tasks": ["8.3"] }
  ]
}
```
