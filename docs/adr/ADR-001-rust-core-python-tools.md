# ADR-001: Rust core with Python tools

**Status:** Accepted (2026-09-30)

## Context

Nebula has two very different kinds of code:

1. **The core**: a long-running daemon, concurrency (model streaming, tool processes, the terminal UI, monitoring), process supervision, sandboxing via Win32 APIs, structured logging, and IPC. It needs to be correct, fast, crash-resistant and low-overhead, and it changes slowly.
2. **The periphery**: tools, workflows, research scrapers and integrations. It changes constantly, and **Nebula itself will write most of it**. It needs to be easy for a local 27B model to write correctly, quick to iterate on, and backed by a large ecosystem.

## Decision

Rust for the core, Python for the periphery. The boundary between them is **MCP** (Model Context Protocol) over stdio/JSON-RPC, a standard, language-neutral protocol.

## Options considered

| Option | Pros | Cons | Verdict |
| --- | --- | --- | --- |
| **Rust core + Python tools** | Best runtime performance and memory safety in the core; excellent Windows API access (`windows-rs`); strong async (`tokio`); Python periphery is the language models write best; huge ecosystem (Playwright, parsing, ML) | Two languages; Rust compiles slowly; LLMs make more mistakes in Rust (borrow checker) | **Chosen.** The core changes rarely and is human-reviewed (layered self-modification), so Rust's difficulty for the model matters least exactly where Rust is used. |
| Go core + Python tools | Fast compiles; simpler language the model writes well; good concurrency | GC pauses (minor); weaker Win32 ergonomics; less expressive types for the protocol and state machines | Strong runner-up. Revisit if Rust slows bootstrap too much. |
| Python only | Fastest bootstrap; one language; the model can self-edit everything | GIL/perf limits in the orchestrator; weaker robustness for a long-running daemon; packaging/distribution pain on Windows; dynamic typing hides errors in self-modified code | Rejected for the core. |
| TypeScript / Node | Best MCP ecosystem; good async | Heavier runtime; weaker systems access; not the owner's background | Rejected. |

## Consequences

- Self-modification in v1 targets Python and config (design doc Section 11). The Rust core gets **proposals** that the owner reviews. This matches both the language split and the trust model.
- The protocol between core and tools is versioned and schema-checked, so the model can't break the core by writing a bad tool.
- Rust compile times are managed with a workspace of small crates, `sccache`, a fast linker (`rust-lld`), and incremental builds in dev.
- Python environments are managed with `uv`, which is fast, lockfile-based and Windows-friendly. Each tool server gets an isolated environment or a shared, locked environment.
