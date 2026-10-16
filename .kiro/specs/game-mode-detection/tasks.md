# Tasks: Game Mode Detection

- [x] 1. Configuration & Wire Protocol
  - [x] 1.1 Add `[game_mode]` section to `config/default.toml`.
  - [x] 1.2 Define `GameModeConfig` in `crates/nebula-config/src/lib.rs` with `deny_unknown_fields` and tests.
  - [x] 1.3 Define `Method::GameModeSet`, `Method::GameModeStatus`, and `Event::GameModeChanged` in `crates/nebula-proto`.
  - [x] 1.4 Add fixture tests and snapshots for new messages in `crates/nebula-proto`.

- [x] 2. Win32 Foreground Detection Seam
  - [x] 2.1 Update `crates/nebula-daemon/Cargo.toml` with Win32 features (`Win32_UI_WindowsAndMessaging`, `Win32_Graphics_Gdi`).
  - [x] 2.2 Implement `ForegroundDetector` trait and Win32 implementation in `crates/nebula-daemon/src/win.rs`.
  - [x] 2.3 Implement mock `ForegroundDetector` for tests.

- [x] 3. Game Mode Engine & Coordinator
  - [x] 3.1 Implement `GameModeCoordinator` in `crates/nebula-daemon/src/game_mode.rs`.
  - [x] 3.2 Implement detection logic (fullscreen, known process list, VRAM threshold, desktop allowlist filter).
  - [x] 3.3 Implement cooldown timer before resuming.
  - [x] 3.4 Implement model unload on game mode entry and reload on exit.
  - [x] 3.5 Unit tests for detection and state transitions.

- [x] 4. Executor & Server Integration
  - [x] 4.1 Wire `GameModeCoordinator` into `Shared` in `crates/nebula-daemon/src/lib.rs`.
  - [x] 4.2 Add `wait_if_active` check inside `execute_loop` in `crates/nebula-daemon/src/executor.rs`.
  - [x] 4.3 Handle `Method::GameModeSet` and `Method::GameModeStatus` in `crates/nebula-daemon/src/server.rs`.

- [x] 5. CLI Implementation
  - [x] 5.1 Implement `nebula game-mode on|off|status` in `crates/nebula-cli`.
  - [x] 5.2 Implement `nebula resume` in `crates/nebula-cli`.
  - [x] 5.3 CLI parser tests in `crates/nebula-cli/tests/cli.rs`.

- [x] 6. Verification & Quality Gates
  - [x] 6.1 Integration tests in `crates/nebula-daemon/tests/` verifying model unload, task pause, and resume.
  - [x] 6.2 Pass `cargo fmt --all`.
  - [x] 6.3 Pass `cargo clippy --workspace --all-targets --locked -- -D warnings`.
  - [x] 6.4 Pass `cargo nextest run --workspace --locked`.
  - [x] 6.5 Pass bench tests in `bench/`.
  - [x] 6.6 Update `docs/ARCHITECTURE.md`.
