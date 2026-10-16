# Requirements: Basic TUI (ratatui)

## 1. Overview
A terminal UI client for Nebula connecting over the Windows named pipe (ADR-007), providing real-time task visualization, live streaming model output, tool-call tracking, hardware resource telemetry, and an interactive approval modal for high-tier actions. Designed for both desktop Windows Terminal and remote SSH sessions from laptop (macOS Terminal).

## 2. Functional Requirements
1. **Panes & Layout:**
   - **Task List:** Shows active, completed, failed, and cancelled tasks with status badges and summaries.
   - **Streaming Model Output:** Live view of model reasoning and tokens emitted for running tasks or chats.
   - **Tool-Call View:** Live list of tool calls made by the agent with tool names, parameters preview, duration, and status.
   - **Resource Bar:** Real-time hardware telemetry: VRAM (used / total), commit charge (used / limit), GPU utilization %, model state (Ready, Loading, Unloaded), and Game Mode status.
2. **Interactive Approval Modal:**
   - Triggered when `Event::ApprovalNeeded` is received from the daemon.
   - Displays modal overlay containing command name, requested tier, rule ID, trace ID, and human-readable explanation.
   - Hotkeys: `[a]` / `[Enter]` to approve, `[d]` / `[Esc]` to deny, `[v]` to toggle detailed view.
   - Approval/Denial sends `approval.respond` IPC request to daemon, which stores the authorizing grant in `ApprovalStore` and emits structured telemetry.
3. **SSH & Terminal Ergonomics:**
   - Works over SSH without mouse dependence (pure keyboard navigation: `Tab` to cycle focus, arrows to scroll, hotkeys for actions, `q` to quit).
   - Robust terminal cleanup on normal exit, error, or panic via RAII drop guards.
   - Handles terminal resize dynamically without crashing or tearing.
4. **Wire Protocol & IPC:**
   - Implements `Method::ApprovalRespond` with parameters `ApprovalRespondParams { trace_id, approved, command, tier, reason }`.
   - Handled in `nebula-daemon` to record decision in `ApprovalStore` and log `permit.decision`.
5. **Testing & Snapshots:**
   - Layout snapshot tests using `ratatui::backend::TestBackend` and `insta` asserting text buffer rendering for main dashboard and approval modal.
   - Unit tests for TUI state transitions, event processing, and keyboard navigation.
