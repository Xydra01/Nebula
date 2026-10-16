# Tasks: Basic TUI (ratatui)

- [ ] 1. Dependencies Setup
  - [ ] 1.1 Add `ratatui` and `crossterm` to workspace dependencies in root `Cargo.toml`.
  - [ ] 1.2 Add `ratatui` and `crossterm` to `crates/nebula-cli/Cargo.toml`.

- [ ] 2. Protocol & Wire Methods
  - [ ] 2.1 Add `Method::ApprovalRespond` and types `ApprovalRespondParams`, `ApprovalRespondResponse` to `crates/nebula-proto/src/methods.rs`.
  - [ ] 2.2 Update `Method::NAMES` and export types in `crates/nebula-proto/src/lib.rs`.
  - [ ] 2.3 Add wire fixtures and update snapshot tests in `crates/nebula-proto/tests/messages.rs`.

- [ ] 3. Daemon Approval Store & Dispatch
  - [ ] 3.1 Wire a shared `ApprovalStore` into `Shared` state in `crates/nebula-daemon/src/lib.rs`.
  - [ ] 3.2 Implement `Method::ApprovalRespond` in `crates/nebula-daemon/src/server.rs` with `permit.decision` telemetry logging.
  - [ ] 3.3 Add daemon integration test for approval respond IPC in `crates/nebula-daemon/tests/ipc.rs`.

- [ ] 4. TUI Core Architecture in `nebula-cli`
  - [ ] 4.1 Create `crates/nebula-cli/src/tui/app.rs` with `TuiApp` state, pane management, and event handling.
  - [ ] 4.2 Create `crates/nebula-cli/src/tui/ui.rs` implementing ratatui widgets for resource bar, tasks, streaming output, tool calls, and approval modal.
  - [ ] 4.3 Create `crates/nebula-cli/src/tui/terminal.rs` with RAII `TerminalGuard` for raw mode and alternate screen setup/restore.
  - [ ] 4.4 Create `crates/nebula-cli/src/tui/event.rs` combining pipe event receiver, periodic tick, and crossterm key input.

- [ ] 5. CLI Command & Integration
  - [ ] 5.1 Add `Command::Tui` to `nebula-cli::Command`.
  - [ ] 5.2 Wire default interactive invocation in `crates/nebula-cli/src/main.rs`.
  - [ ] 5.3 Implement `nebula_cli::tui::run` executing the event loop.

- [ ] 6. Snapshots & Quality Gates
  - [ ] 6.1 Add headless snapshot tests in `crates/nebula-cli/tests/tui.rs` using `TestBackend` for main dashboard, streaming tokens, and approval modal.
  - [ ] 6.2 Run `cargo fmt --all`.
  - [ ] 6.3 Run `cargo clippy --workspace --all-targets --locked -- -D warnings`.
  - [ ] 6.4 Run `cargo nextest run --workspace --locked`.
  - [ ] 6.5 Run python benchmark tests (`ruff`, `pytest`).
  - [ ] 6.6 Update `docs/ARCHITECTURE.md`.
