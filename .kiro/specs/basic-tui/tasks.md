# Tasks: Basic TUI (ratatui)

- [x] 1. Dependencies Setup
  - [x] 1.1 Add `ratatui` and `crossterm` to workspace dependencies in root `Cargo.toml`.
  - [x] 1.2 Add `ratatui` and `crossterm` to `crates/nebula-cli/Cargo.toml`.

- [x] 2. Protocol & Wire Methods
  - [x] 2.1 Add `Method::ApprovalRespond` and types `ApprovalRespondParams`, `ApprovalRespondResponse` to `crates/nebula-proto/src/methods.rs`.
  - [x] 2.2 Update `Method::NAMES` and export types in `crates/nebula-proto/src/lib.rs`.
  - [x] 2.3 Add wire fixtures and update snapshot tests in `crates/nebula-proto/tests/messages.rs`.

- [x] 3. Daemon Approval Store & Dispatch
  - [x] 3.1 Wire a shared `ApprovalStore` into `Shared` state in `crates/nebula-daemon/src/lib.rs`.
  - [x] 3.2 Implement `Method::ApprovalRespond` in `crates/nebula-daemon/src/server.rs` with `permit.decision` telemetry logging.
  - [x] 3.3 Add daemon integration test for approval respond IPC in `crates/nebula-daemon/tests/ipc.rs`.

- [x] 4. TUI Core Architecture in `nebula-cli`
  - [x] 4.1 Create `crates/nebula-cli/src/tui/app.rs` with `TuiApp` state, pane management, and event handling.
  - [x] 4.2 Create `crates/nebula-cli/src/tui/ui.rs` implementing ratatui widgets for resource bar, tasks, streaming output, tool calls, and approval modal.
  - [x] 4.3 Create `crates/nebula-cli/src/tui/terminal.rs` with RAII `TerminalGuard` for raw mode and alternate screen setup/restore.
  - [x] 4.4 Create `crates/nebula-cli/src/tui/event.rs` combining pipe event receiver, periodic tick, and crossterm key input.

- [x] 5. CLI Command & Integration
  - [x] 5.1 Add `Command::Tui` to `nebula-cli::Command`.
  - [x] 5.2 Wire default interactive invocation in `crates/nebula-cli/src/main.rs`.
  - [x] 5.3 Implement `nebula_cli::tui::run` executing the event loop.

- [x] 6. Snapshots & Quality Gates
  - [x] 6.1 Add headless snapshot tests in `crates/nebula-cli/tests/tui.rs` using `TestBackend` for main dashboard, streaming tokens, and approval modal.
  - [x] 6.2 Run `cargo fmt --all`.
  - [x] 6.3 Run `cargo clippy --workspace --all-targets --locked -- -D warnings`.
  - [x] 6.4 Run `cargo nextest run --workspace --locked`.
  - [x] 6.5 Run python benchmark tests (`ruff`, `pytest`).
  - [x] 6.6 Update `docs/ARCHITECTURE.md`.

- [ ] 7. TUI Enhancements for Language Modules, Diagnostics, and Executor Lifecycle (#31)
  - [ ] 7.1 Collapsible structured diagnostic sub-pane rendering compiler and build errors from `CppAdapter` / `nebula-devtools` (#70).
  - [ ] 7.2 Visual lifecycle badges for `task.finish` invocation, auto-commit state, and `limit_reached` ceiling exhaustion (#69, #72).
  - [ ] 7.3 Active language module badge and MCP process health status in header (#62, #64, #73).
  - [ ] 7.4 Amber Game Mode pause banner reflecting `Event::GameModeChanged` (#35).
  - [ ] 7.5 Approval modal displays rule violation context and command classification (#71, #74).
  - [ ] 7.6 Snapshot tests in `crates/nebula-cli/tests/tui.rs` updated for all new widget states.

