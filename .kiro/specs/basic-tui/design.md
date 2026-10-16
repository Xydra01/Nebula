# Design: Basic TUI (ratatui)

## 1. Architecture

```
   ┌────────────────────────────────────────────────────────┐
   │                       nebula CLI                       │
   │  ┌──────────────────────────────────────────────────┐  │
   │  │             TuiApp (State Machine)               │  │
   │  │  - tasks, streaming tokens, tools, resources    │  │
   │  │  - active pane, scroll, approval modal state     │  │
   │  └────────▲─────────────────────────────┬───────────┘  │
   │           │ Events                      │ Actions      │
   │  ┌────────┴──────────┐        ┌─────────▼───────────┐  │
   │  │   Event Listener  │        │   Crossterm & UI    │  │
   │  │   - pipe events   │        │   - ratatui render  │  │
   │  │   - periodic tick │        │   - key input loop  │  │
   │  └────────▲──────────┘        └─────────────────────┘  │
   └───────────┼────────────────────────────────────────────┘
               │ JSON-RPC IPC (\\.\pipe\nebula)
   ┌───────────▼────────────────────────────────────────────┐
   │                     nebula-daemon                      │
   │  - Method::ApprovalRespond -> records in ApprovalStore │
   │  - Events: TaskStep, ChatToken, ApprovalNeeded, etc.   │
   └────────────────────────────────────────────────────────┘
```

## 2. Dependencies
- Root `Cargo.toml`:
  - `ratatui = { version = "0.30.2", default-features = false, features = ["crossterm"] }`
  - `crossterm = "0.29.0"`
- `crates/nebula-cli/Cargo.toml`:
  - `ratatui.workspace = true`
  - `crossterm.workspace = true`

## 3. Wire Protocol & Daemon Integration
- New IPC method: `Method::ApprovalRespond(ApprovalRespondParams) -> ApprovalRespondResponse`
- Parameters:
  ```rust
  pub struct ApprovalRespondParams {
      pub trace_id: String,
      pub approved: bool,
      pub command: String,
      pub tier: String,
      pub reason: Option<String>,
  }
  pub struct ApprovalRespondResponse {
      pub success: bool,
      pub message: String,
  }
  ```
- Handled in `crates/nebula-daemon/src/server.rs`:
  - On `approved = true`: mints `GrantId::new(unique_id)`, constructs `Approval::grant`, stores it in the daemon's `ApprovalStore`, and emits `permit.decision` telemetry event with `approved = true`.
  - On `approved = false`: logs `permit.decision` telemetry with `approved = false`.

## 4. UI Layout & Panes
- Top bar: `Header` showing Nebula version, Daemon uptime, and Model Profile + State.
- Resource strip: Gauges/indicators for VRAM usage (used/total), RAM commit, GPU %, and Game Mode status.
- Main body (split layout):
  - Left column:
    - Upper: Tasks List (`List` widget with status indicators `[RUNNING]`, `[COMPLETED]`, `[FAILED]`).
    - Lower: Tool Calls (`Table` widget showing call name, args, status).
  - Right column:
    - Streaming Token & Model Output (`Paragraph` with scroll support).
- Bottom bar: Status & hotkey help (`[q] Quit  [Tab] Focus  [a] Approve  [d] Deny  [↑/↓] Navigate`).
- Modal Overlay:
  - Triggered by `Event::ApprovalNeeded`.
  - Centered popup window (`Clear` + `Block` with double border).
  - Prominently displays command, tier, rule ID, and trace ID.
  - Interactive approval actions.

## 5. Terminal Lifecycle & SSH Safety
- `TerminalGuard` RAII struct:
  - Enters raw mode and alternative screen on creation.
  - Leaves alternative screen and disables raw mode on drop (even in panics).
- Works with 80x24 terminal minimum, scales smoothly to wide screens.
- Avoids mouse capture so text can still be selected via terminal emulator over SSH.

## 6. Testing Strategy
- `ratatui::backend::TestBackend` allows deterministic headless rendering.
- `insta::assert_snapshot!` verifies rendered text buffers across states:
  - Default initial dashboard.
  - Dashboard with active streaming text and multiple tasks.
  - Active approval modal overlay.
