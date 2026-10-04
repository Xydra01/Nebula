# Nebula

Nebula is a **local-first AI coding agent** that runs entirely on a single consumer PC (RTX 4070 12 GB, 32 GB RAM). It takes a software task from idea to working, tested code using a local model, Ternary Bonsai 2 27B served by llama.cpp. Cloud AI is used only through explicit, approved tool calls. Nebula is environment-aware (it budgets VRAM, RAM, disk and context), can research the web, has deep but guarded command-line access, and logs everything it does.

It is the first app of a planned local **AI foundry**, and its long-term job is to build the other apps and, step by step, improve itself.

## Status

**Phase 0 (foundations) is built; the exit review is pending.** Today Nebula is a supervised local model with a CLI. It isn't an agent yet:

- a daemon that runs and supervises the model servers (crash restart, profile switching, Job Object cleanup);
- `nebula chat` with streaming;
- structured logs with trace IDs;
- `nebula doctor` health checks;
- encrypted off-site backups.

**Phase 1** adds the agent: tools over MCP, permission tiers, worktrees, an executor loop and a TUI. See the [`phase-1` issues](https://github.com/Xydra01/Nebula/issues?q=is%3Aissue+label%3Aphase-1).

## Quick start (on the Nebula machine)

```powershell
.\scripts\install-nebula.ps1     # build and install nebula.exe + nebula-daemon.exe to F:\Nebula\bin
nebula daemon start               # load the default model profile
nebula chat                       # /reset, /exit; Ctrl-C cancels a reply
nebula doctor                     # health report
nebula daemon stop                # free the GPU
```

`nebula --help` lists everything. Setting up a machine from scratch (runtimes, models, toolchain): [docs/ops/setup.md](docs/ops/setup.md).

## Documents

- [AGENTS.md](AGENTS.md): start here if you're an AI agent or new to the repo. Build/test commands, conventions, hard rules and gotchas.
- [Architecture and code map](docs/ARCHITECTURE.md): what's built, the crates, the on-disk layout, and the docs index.
- [Design document](docs/NEBULA_DESIGN.md): the target architecture, agent design, security model, self-improvement framework, roadmap and risks.
- [Phase 0 plan](docs/PHASE0_PLAN.md): workstreams, "As built" notes and the exit criteria.
- [ADRs](docs/adr/README.md): recorded decisions.
- [Ops docs](docs/ops/): setup, backups, runbooks, and the machine log ([phase0-notes.md](docs/ops/phase0-notes.md)).
- [Contributing](CONTRIBUTING.md): branches, commits, PRs, Rust conventions.

## Stack

- **Core:** Rust 2024 (tokio, tracing, serde, clap, windows-rs, nvml-wrapper). Later: ratatui, rusqlite + sqlite-vec, rmcp.
- **Periphery:** Python (uv). Today that's the benchmark harness in `bench/`; Phase 1 adds the MCP tool servers.
- **Tool boundary:** MCP ([ADR-002](docs/adr/ADR-002-mcp-tool-boundary.md)).
- **Model runtime:** the PrismML llama.cpp fork, plus stock llama.cpp for the fallback and embedding models, supervised as child processes ([ADR-003](docs/adr/ADR-003-llama-cpp-backend.md)).

## License

[MIT](LICENSE)
