# nebula-devtools

Nebula's **first external Python MCP tool server** (GitHub issue #33), and the reusable template
for future Python tools (ADR-001, ADR-002). It exposes build/test/lint adapters as MCP tools:

- **`test.run`** — run the test runner and return per-test pass/fail with `file:line` failures.
- **`build.run`** — run the build/compile step and return compiler diagnostics.
- **`lint.run`** — run the linter and return findings.

Languages (v1): **Python** via `uv`/`pytest` (test) and `ruff` (lint); **Rust** via `cargo build`
(build), `cargo nextest` (test), and `cargo clippy` (lint). `pyright` is deferred (it needs Node).

## How it is run

This is a stdio MCP server: it speaks newline-delimited JSON-RPC 2.0 (protocol `2025-06-18`) on
**stdin/stdout**, and the daemon's tool host launches it, performs the `initialize` / `tools/list`
handshake, and routes `tools/call`. The host owns the per-call timeout, the output cap (over-cap
output spills to a blob), worktree confinement, and the `tool.call` telemetry event — this server
implements none of that.

**Permission tier (for the executor, #32).** `test.run`/`build.run`/`lint.run` are intended to run
at **Sandbox (Tier 1)** so the executor can run tests autonomously without an approval, since every
call is confined to the task's worktree and bounded by the task's kill-on-close Job Object (#30).
Note the `nebula-sandbox` classifier only tiers *shell commands* by base name; it does not see MCP
tool names. Assigning this Sandbox tier to the three tools is the executor's job and lands with
issue #32 (external-tool tiering), so no `rules.toml` change is made here.

**stdout is the protocol channel.** The server writes only JSON-RPC objects (one per line) to
stdout; all logs, diagnostics, and runner chatter go to **stderr**.

Enable it by adding a `[tools.servers.devtools]` entry to the local config override (see the
commented example in `config/default.toml`):

```toml
[tools.servers.devtools]
command = "uv"
args = ["run", "--project", 'F:\...\tools\nebula-devtools', "nebula-devtools"]
call_timeout_ms = 120000
max_output_bytes = 1048576
env = [["NEBULA_RETIRED_DRIVE", "C:"]]
```

## Development

```
uv run ruff check .
uv run ruff format --check .
uv run pytest -q
```

The parser and MCP-loop tests need no external toolchain (they read captured fixtures and drive the
loop over pipes). The end-to-end adapter tests require `uv`/`cargo` and skip cleanly when a runner
is absent.
