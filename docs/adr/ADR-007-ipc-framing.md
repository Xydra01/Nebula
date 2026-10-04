# ADR-007: IPC framing: NDJSON JSON-RPC over a named pipe

**Status:** Accepted (2026-10-04)

## Context

The daemon (`nebula-daemon`) is long-running. Every front end talks to it: the CLI now, and later the TUI and the `nebula mcp` bridge for Cursor (design Section 4.3). They need a local transport and a message format that:

- stays on this machine and is usable only by the logged-in user, including from an SSH session as the same user;
- carries request/response calls **and** an unsolicited event stream (chat tokens, model state, log events) on one connection;
- is easy to debug by eye and to drive from Python tools and tests;
- lets the CLI and daemon be upgraded separately without silent misbehaviour.

The design doc had already chosen JSON-RPC 2.0 over a named pipe. This ADR records the framing and the details the implementation settled on in WS4.

## Decision

- **Transport:** the Windows named pipe `\\.\pipe\nebula` (`daemon.pipe_name` in config).
  - The pipe's security descriptor is `D:P(A;;GA;;;<current user SID>)`: only the user who started the daemon can open it, not even administrators.
  - Remote clients are rejected at the pipe level. Remote use goes through SSH as the same user.
  - A named mutex allows a single daemon. A second instance exits with code 3.
- **Framing: newline-delimited JSON (NDJSON).**
  - Each message is one JSON object followed by `\n`. JSON string escaping guarantees there is no other newline inside a message.
  - Lines are capped at 16 MiB. An over-long line gets an error response, then the connection is closed, because the stream can't be resynchronised reliably.
- **Messages: JSON-RPC 2.0**, with three kinds told apart by their members:
  - **Request:** `id` + `method` + `params`.
  - **Response:** `id` + `result`/`error`.
  - **Notification:** `method` alone. These are daemon events such as `chat.token`, `chat.done`, `model.state_changed` and `log.event`.

  Responses and notifications interleave on the same connection, so clients must skip notifications while waiting for a response.
- **Versioning:**
  - Every message carries `proto_version` (currently 1), and it is checked **before** the payload is parsed.
  - A mismatch returns error `-32000 VERSION_MISMATCH` instead of a confusing parameter error.
  - Any incompatible change to a message bumps the version.
- **Errors:** the standard JSON-RPC codes, plus Nebula's in `-32000..-32099`: version mismatch, model unavailable, busy, cancelled, shutting down, not found.
- **Tracing:** requests may carry a `trace_id` (a ULID). The daemon attaches the work to it, or starts a new trace, so a CLI call can be followed through the logs.
- **Methods (Phase 0):** `daemon.status`, `daemon.shutdown`, `chat.start`, `chat.cancel`, `model.status`, `model.set_profile`, `resources.snapshot`, `logs.subscribe` and `doctor.run`.
- **Tests:** golden fixtures in `nebula-proto` pin the wire format. The daemon's IPC tests run the real server over a real pipe.

## Options considered

- **Length-prefixed frames** (a 4-byte length, then JSON or a binary body). These are robust for binary payloads, but you can't read or write them by hand, and every client needs custom framing code. NDJSON works with any line reader. Nebula's payloads are JSON text, so binary data never needs to cross the pipe.
- **`Content-Length` headers (LSP style).** Self-describing, but there's more parsing and nothing to gain over NDJSON for messages that never contain raw newlines.
- **Local HTTP/WebSocket or gRPC on localhost.** These are familiar tools, but they open a TCP port. A port has to be protected from other local users and kept off the network, whereas the pipe's ACL does that for free. gRPC would also add protobuf code generation and a heavy dependency for a handful of methods.
- **A binary encoding** (MessagePack, CBOR). Smaller and faster, but harder to debug. The pipe isn't a bottleneck, since streaming runs at about 70 tokens/s.

## Consequences

- Any language with a JSON library and a way to open a file can talk to the daemon. A Python tool or a test can drive it in a few lines.
- Because notifications interleave with responses, every client needs a small loop that matches responses by `id`. This caused one flaky test in WS4, now fixed with a shared helper.
- The 16 MiB line cap bounds memory per connection. Large artifacts (prompts, outputs, files) travel by reference as log blobs or paths, never inline.
- Phase 1 methods (`task.*`, `approval.*`, `config.*`) are added to the `Method` enum. Additive changes keep `proto_version` 1; renaming or removing a field bumps it.
- The pipe stays local-only. Remote access is always SSH, Remote-SSH or the tailnet-only approval page (design Section 4.5), never a port.
