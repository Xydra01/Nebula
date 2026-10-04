# Contributing to Nebula

Nebula is built by one human owner (@Xydra01) and, increasingly, by Nebula itself through the `Nebula-dev-bot` account. These rules apply to both. Outside contributions are welcome via issues; open one before sending a large PR.

## Setup

1. Install the toolchain listed in [PHASE0_PLAN.md](docs/PHASE0_PLAN.md) WS1 (Rust stable/MSVC, uv, gitleaks, Node LTS).
2. Install the git hooks once per clone:

   ```powershell
   powershell -ExecutionPolicy Bypass -File scripts/install-hooks.ps1
   ```

   This sets `core.hooksPath=.githooks`. The pre-commit hook scans staged changes and the pre-push hook scans outgoing commits, both with gitleaks. Don't bypass them with `--no-verify`. If one flags a false positive, add a narrowly scoped entry to `.gitleaksignore` in the same PR and explain why.

## Branches

| Who | Pattern | Example |
| --- | --- | --- |
| Nebula (bot) | `nebula/<task-id>-<slug>` | `nebula/t_0042-tool-timeouts` |
| Human, feature | `feat/<slug>` | `feat/ws2-repo-hygiene` |
| Human, fix | `fix/<slug>` | `fix/pipe-reconnect` |
| Human, docs/chore | `docs/<slug>`, `chore/<slug>` | `docs/adr-004` |

`main` is protected: every change goes through a PR, force-pushes and deletion are blocked, and merged branches are deleted automatically.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/): `type(scope): summary`, written in the imperative and at most 72 characters.

- Types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`.
- Scope is optional and usually a crate or area: `feat(model): ...`, `fix(daemon): ...`, `docs(adr): ...`.
- Explain **why** in the body, not what the diff already shows.

**Commits produced by Nebula** are authored under the owner's account and must carry both trailers:

```
Nebula-Task: t_0042
Co-authored-by: Nebula-dev-bot <336789866+Nebula-dev-bot@users.noreply.github.com>
```

This lets `git log --grep "Nebula-Task"` and GitHub's co-author display show exactly what Nebula built. Human-only commits omit them.

## Pull requests

- Keep PRs small and focused on one task. Link the issue or `Nebula-Task` id in the description.
- **Review:** `CODEOWNERS` requires an approving review from @Xydra01, and the approval must come after the latest push. Bot PRs are approved by the owner, and the bot never approves anything. The owner's own PRs are merged using the repository-admin bypass.
- **Merging:** use merge or squash. Rebase-merge is disabled.
- **Self-modification:** PRs that touch the protected set (core crates, policy, permission tiers, the benchmark harness) are always human-reviewed and never auto-merged, whatever the repo's `trust_level`. See the design doc, Section 11.

## Checks to run locally

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs these on every PR; the required check is **CI ok**. The Rust checks are skipped until the workspace has a `Cargo.toml`.

```powershell
gitleaks git --redact
cd bench; uv run ruff check .; uv run ruff format --check .; uv run pytest -q; cd ..
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
```

Tests that need the GPU or the real model are marked `#[ignore]` (tagged `gpu`) and run only locally, with `cargo nextest run --run-ignored all`. CI runners have no GPU, and the project deliberately does not use a self-hosted runner.

## Design decisions

Significant decisions are recorded as ADRs in [docs/adr/](docs/adr/). To propose one, open an issue with the **ADR proposal** template, then add `docs/adr/ADR-NNN-<slug>.md` in a PR.
