"""Nebula devtools: the first external Python MCP tool server (issue #33).

Exposes ``test.run``, ``build.run``, and ``lint.run`` for Python (uv/pytest/ruff) and Rust
(cargo build/nextest/clippy) over MCP (newline-delimited JSON-RPC 2.0 on stdin/stdout). The
daemon's tool host launches this server in a Job Object and owns the timeout, output cap, blob
spill, permission tier, worktree confinement, and telemetry; this package implements only the
server and its language adapters.
"""

__version__ = "0.1.0"
