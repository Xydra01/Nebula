# Nebula

Nebula is a **local-first AI coding agent** that runs entirely on a single consumer PC (RTX 4070 12 GB, 32 GB RAM). It takes a software task from idea to working, tested code using a local model, Ternary Bonsai 2 27B served by llama.cpp. Cloud AI is used only through explicit, approved tool calls. Nebula is environment-aware (it budgets VRAM, RAM, disk and context), can research the web, has deep but guarded command-line access, and logs everything it does.

It is the first app of a planned local **AI foundry**, and its long-term job is to build the other apps and, step by step, improve itself.

## Status

**Phase 0: foundations (pre-alpha).** Design is complete; machine prep is underway; there is no runnable code yet.

## Documents

- [Design document](docs/NEBULA_DESIGN.md): architecture, agent design, security model, self-improvement framework, roadmap, risks
- [Phase 0 plan](docs/PHASE0_PLAN.md): workstreams, milestones and exit criteria for the first ~6 weeks
- [Phase 0 ops notes](docs/ops/phase0-notes.md): machine-prep results

## Stack (planned)

- **Core:** Rust (tokio, tracing, ratatui, rusqlite + sqlite-vec, nvml-wrapper, rmcp)
- **Periphery:** Python (uv, MCP SDK, Playwright, pytest)
- **Tool boundary:** MCP
- **Model runtime:** PrismML llama.cpp fork, supervised as a child process

## License

[MIT](LICENSE)
