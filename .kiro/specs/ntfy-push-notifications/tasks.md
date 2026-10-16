# Tasks: ntfy Push Notifications over Tailscale

- [ ] 1. Configuration & Wire Protocol
  - [ ] 1.1 Add `[notifications]` section to `config/default.toml` and add `"nebula/ntfy_token"` to `daemon.secrets`.
  - [ ] 1.2 Implement `NotificationsConfig` in `crates/nebula-config/src/lib.rs` with `deny_unknown_fields` and tests.
  - [ ] 1.3 Add wire methods `Method::NotifyStatus` and `Method::NotifyTest` to `crates/nebula-proto`.
  - [ ] 1.4 Add fixture snapshots for new wire messages in `crates/nebula-proto`.

- [ ] 2. Tailscale Confinement & Notification Engine
  - [ ] 2.1 Implement `is_tailscale_or_local` validation function and unit tests.
  - [ ] 2.2 Implement `NotificationClient` / `NotificationService` in `crates/nebula-daemon/src/notifications.rs`.
  - [ ] 2.3 Implement secret redaction, body length limits, and ntfy header formatting.
  - [ ] 2.4 Support credential retrieval for `token_target` via Windows Credential Manager.

- [ ] 3. Daemon Integration & Event Triggers
  - [ ] 3.1 Wire `NotificationService` into `Shared` in `crates/nebula-daemon/src/lib.rs`.
  - [ ] 3.2 Dispatch notifications on task completion / failure in `crates/nebula-daemon/src/executor.rs`.
  - [ ] 3.3 Dispatch notifications when circuit breaker trips in `executor.rs`.
  - [ ] 3.4 Dispatch notifications when approval is needed in `builtin_providers.rs` / `executor.rs`.
  - [ ] 3.5 Implement periodic doctor check monitor in `nebula-daemon` and alert on newly failing checks.
  - [ ] 3.6 Handle `Method::NotifyStatus` and `Method::NotifyTest` in `crates/nebula-daemon/src/server.rs`.

- [ ] 4. CLI Commands & Doctor Check
  - [ ] 4.1 Implement `nebula notify status` and `nebula notify test` in `crates/nebula-cli`.
  - [ ] 4.2 Add CLI formatting and parser tests.
  - [ ] 4.3 Add `"notifications"` health check to `nebula doctor`.

- [ ] 5. Testing & Quality Gates
  - [ ] 5.1 In-process integration tests with an `axum` test server verifying ntfy POST delivery, headers, and rejection of non-Tailscale targets.
  - [ ] 5.2 Pass `cargo fmt --all`.
  - [ ] 5.3 Pass `cargo clippy --workspace --all-targets --locked -- -D warnings`.
  - [ ] 5.4 Pass `cargo nextest run --workspace --locked`.
  - [ ] 5.5 Pass `cd bench; uv run ruff check .; uv run ruff format --check .; uv run pytest -q`.
  - [ ] 5.6 Update `docs/ARCHITECTURE.md`.
