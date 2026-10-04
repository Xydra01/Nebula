# AGENTS.md

Orientation and ground rules for AI coding agents (and humans new to the repo). Read this first, then [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the code map.

## What this is

Nebula is a local-first AI coding agent for one Windows PC (RTX 4070 12 GB, 32 GB RAM). **Phase 0 (foundations) is built:**

- a daemon that supervises local llama.cpp model servers;
- a `nebula` CLI (chat, model, logs, resources, doctor, backup);
- structured telemetry;
- encrypted backups.

**Phase 1** (the agent itself: tools over MCP, permission tiers, worktrees, an executor loop, a TUI) is planned as GitHub issues labelled `phase-1`.

## Where things are

- `crates/`: the Rust workspace (8 crates). The table in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) says what each one does and where to make common changes.
- `config/`: `default.toml` (embedded defaults), plus the runtime and model lock files.
- `bench/`: the Python benchmark and smoke-test harness (uv).
- `scripts/`: PowerShell ops scripts.
- `docs/`: the design doc, the Phase 0 plan, ADRs and ops runbooks. The docs index is at the bottom of ARCHITECTURE.md.

## Build and test

The shell is Windows with the MSVC Rust toolchain (`rust-toolchain.toml`). Build output goes to `CARGO_TARGET_DIR=F:\Nebula\build\target`, which is set in the user environment, with sccache.

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked             # the same as CI
cd bench; uv run ruff check .; uv run ruff format --check .; uv run pytest -q
```

- **All of these must pass before a PR.** CI runs them on Windows; the required check is **CI ok**.
- **GPU and real-machine tests** are `#[ignore]`d. Run them locally with `cargo nextest run --run-ignored all`, and only when the model isn't needed for something else.
- **Snapshot tests** (`insta`): the wire format in `crates/nebula-proto/tests/snapshots/` and CLI output in `crates/nebula-cli/tests/snapshots/`. Review the diffs and accept them with `INSTA_UPDATE=always`. An incompatible wire change bumps `PROTO_VERSION`.
- **To try the real binaries:** stop the daemon (`nebula daemon stop`), then run `scripts\install-nebula.ps1`, then `nebula daemon start`. The install fails while the daemon is running, because the exe files are locked.

## Code conventions

These are enforced by the workspace lints in `Cargo.toml` and spelled out in [CONTRIBUTING.md](CONTRIBUTING.md).

- **No panics in library code:** no `unwrap()`, `expect()` or `panic!` outside tests. Integration test files start with `#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]`.
- **Errors:** `thiserror` in libraries; `anyhow` only in the two binaries.
- **Unsafe code:** denied, except in the isolated `win.rs` modules. Each `unsafe` block needs a `// SAFETY:` comment.
- **Documentation:** public items need doc comments. Every crate's `lib.rs` opens with a module doc; keep it accurate.
- **Config:** typed structs with `deny_unknown_fields`. A new key goes in **both** `config/default.toml` and `nebula-config`.
- **Tests never touch real paths.** Redirect `paths.*`, `daemon.pipe_name` and `backup.*` to a `tempfile` dir and a unique pipe name (see `crates/nebula-cli/tests/e2e.rs`). The fake llama-server is in `nebula_model::testing`.
- **Commits:** Conventional Commits (`feat(model): …`), explaining *why*. AI-made commits add the trailer `Co-authored-by: Cursor <cursoragent@cursor.com>`, or the equivalent for your tool.
- **Comments:** only for constraints the code can't show. No narration.

## Hard rules

This is a real personal machine. Breaking these can lose data or leave the PC unable to boot.

1. **Never push to `main`, force-push, or merge PRs.** Work on a branch (`feat/…`, `fix/…`, `docs/…`) and open a PR. The owner (@Xydra01) reviews and merges everything.
2. **Never skip the git hooks** (`--no-verify`). They run gitleaks. Never change git config without asking.
3. **No secrets in the repo, logs, command arguments or chat.** Secrets live in Windows Credential Manager (`nebula/*` targets) and are read at runtime. Never print them. Don't commit personal data such as email addresses or public keys.
4. **Ask before deleting.** Move files rather than delete them. Any delete over **1 GB or 500 files** needs explicit approval. That includes code paths you write: retention and cleanup code must stop at the same limit. The only exception is the log archive's 8 GB auto-prune.
5. **Never touch `C:`.** It's a retired, failing drive. Nothing may be read from or written to it, and no configured path may resolve to it.
6. **Leave the machine setup alone:** boot configuration, partitions, the page file, drivers, firewall rules, scheduled tasks and services, unless the task explicitly asks for it. Those changes are tier 3 and need the human.
7. **Only stop processes you started.** The daemon, llama-server and the backup tasks may be running for the owner. Use `nebula daemon stop` rather than killing processes.
8. **Workflow files** (`.github/workflows/*`) can't be pushed with the bot token. Leave such changes for the owner to push.
9. **Protected set** (core crates, permission policy, the benchmark harness): changes are always human-reviewed (design Section 11.5). Expect close review, and keep those PRs small.

## Gotchas

- **The lock files are compiled into `nebula-daemon`** (`include_str!`). After editing `config/runtime.lock.toml` or `models.lock.toml`, reinstall the binaries, or doctor keeps checking the old pins.
- **`config/default.toml` is embedded** in the binaries too. Local changes on this machine go in `F:\Nebula\config\nebula.toml`, which is outside the repo.
- **Notifications interleave with responses on the pipe.** A client waiting for a response must skip `Message::Notification` (see `next_response()` in `crates/nebula-daemon/tests/ipc.rs`).
- **Detached child processes inherit handles** on Windows unless prevented. `nebula-cli/src/win.rs` clears the inherit flag on the standard handles before spawning the daemon. Keep that in place, or `nebula daemon start | …` and SSH sessions hang.
- **VRAM and commit charge are tight.** One chat profile uses ~9 GB of VRAM and a similar amount of committed RAM. Ollama, LM Studio and other GPU apps are installed on the machine; a load can fail if they're running.
- **Paths assume `F:` (hot) and `D:` (cold).** Use the config values; don't hard-code new paths.
- **The PowerShell 5.1 scripts** use `$ErrorActionPreference = 'Stop'`. Pass `-ErrorAction Stop` explicitly to CIM cmdlets such as `Register-ScheduledTask`, because their errors otherwise don't stop the script.

## Before you start a task

1. Read the relevant issue, and the "As built" note for that area in [docs/PHASE0_PLAN.md](docs/PHASE0_PLAN.md).
2. Check [docs/adr/](docs/adr/README.md). If your change contradicts an ADR, propose a new ADR instead of drifting from it.
3. Keep PRs small, with a summary and a test plan. Update the docs you made stale (ARCHITECTURE.md, the crate's module doc, the ops runbooks) in the same PR.
