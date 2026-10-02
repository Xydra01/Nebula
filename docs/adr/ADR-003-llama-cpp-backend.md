# ADR-003: llama.cpp (PrismML fork) behind a backend trait

**Status:** Accepted (2026-09-30)

## Context

Ternary Bonsai 2 needs the PrismML llama.cpp fork. The fork changes often, CUDA faults can kill a process, and other backends may be wanted later.

## Decision

The model runs as a supervised **`llama-server` child process** (PrismML fork) exposing an OpenAI-compatible HTTP API on localhost. Nebula talks to it through a `ModelBackend` trait.

## Options considered

- **Separate process (chosen):** crash isolation (a CUDA fault does not kill the daemon), easy upgrades of the fork, and one code path that also works for vanilla llama.cpp or other backends later (vLLM on WSL2, a future PrismML runtime, etc.).
- **Linking the library in-process:** lower latency and no HTTP layer, but a crash in CUDA or the fork takes down the daemon, and every fork upgrade means rebuilding Nebula.

## Consequences

- The daemon supervises `llama-server` inside a Job Object: it starts the server, health-checks it, restarts it on failure, and kills it on exit.
- Model profiles (context size, KV cache type, speculative decoding) become launch arguments behind the trait.
- The Ornith-1.0-9B fallback runs on stock llama.cpp through the same trait.
