# Tasks: GitHub Flow — Nebula-dev-bot Branches and PRs

- [x] 1. Configuration & Wire Types
  - [x] 1.1 Add `[github]` section to `config/default.toml` (`enabled`, `repo`, `base_branch`, `branch_prefix`, `token_target`, `bot_username`).
  - [x] 1.2 Add `GitHubConfig` struct with `deny_unknown_fields` to `crates/nebula-config/src/lib.rs`.
  - [x] 1.3 Add optional `create_pr: Option<bool>` to `TaskRunParams` in `crates/nebula-proto/src/methods.rs`.
  - [x] 1.4 Update snapshots if wire format changes.

- [x] 2. Safety Validation Engine
  - [x] 2.1 Implement `safety.rs` with `validate_push_ref(branch, base_branch, branch_prefix)`.
  - [x] 2.2 Refuse `main`, `master`, and base branch.
  - [x] 2.3 Refuse force-push (`+` prefix, `--force`).
  - [x] 2.4 Refuse invalid branch prefix.
  - [x] 2.5 Refuse invalid ref syntax.
  - [x] 2.6 Unit tests for all refusal paths and valid branch names.

- [x] 3. Push Engine with Isolated Authentication
  - [x] 3.1 Implement `push_task_branch(repo_root, branch, remote_url, token)` in `push.rs`.
  - [x] 3.2 Pass token exclusively via child process `GIT_CONFIG_*` extraheader basic auth.
  - [x] 3.3 Ensure refspec is strictly `refs/heads/<branch>:refs/heads/<branch>`.
  - [x] 3.4 Integration test pushing to a local bare git repo (`git init --bare`).

- [x] 4. GitHub REST API Client & PR Formatter
  - [x] 4.1 Implement `format.rs` for PR title, body (summary, trace ID, test results), and co-author trailers.
  - [x] 4.2 Implement `api.rs` for GitHub REST API (`find_open_pr`, `create_pr`, `update_pr_or_comment`).
  - [x] 4.3 Support configurable base URL for local testing with mock HTTP server.
  - [x] 4.4 Mock API tests using `axum` verifying PR creation and follow-up commit handling.

- [x] 5. Daemon Integration & CLI Flags
  - [x] 5.1 Wire GitHub flow into `crates/nebula-daemon/src/executor.rs` upon `TaskStatus::Completed`.
  - [x] 5.2 Read token from Credential Manager via `nebula_daemon::win::read_credential`.
  - [x] 5.3 Add `--pr` and `--no-pr` flags to `nebula task run` in `crates/nebula-cli/src/task.rs`.
  - [x] 5.4 Update task final summary with PR URL when opened.

- [x] 6. Token Redaction & Leakage Test
  - [x] 6.1 Register token target with telemetry redactor if not already present.
  - [x] 6.2 Write automated integration test that runs the flow with a mock token and greps all log output, asserting zero occurrences of the token.

- [x] 7. Verification & Quality Gates
  - [x] 7.1 Run `cargo fmt --all`.
  - [x] 7.2 Run `cargo clippy --workspace --all-targets --locked -- -D warnings`.
  - [x] 7.3 Run `cargo nextest run --workspace --locked`.
  - [x] 7.4 Run bench tests: `cd bench; uv run ruff check .; uv run ruff format --check .; uv run pytest -q`.
  - [x] 7.5 Update `docs/ARCHITECTURE.md`.
