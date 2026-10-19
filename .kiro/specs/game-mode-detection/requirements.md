# Requirements: Game Mode Detection

## 1. Overview
Detect when gaming is active on the host PC and free up the GPU (design Section 2.6).

## 2. Functional Requirements
- **FR1 (Detection):** Detect gaming through:
  - Fullscreen or exclusive-fullscreen foreground application (window rectangle matches or exceeds display monitor).
  - Configurable known active game processes (direct game executables; background store launchers idling in tray do not trigger).
  - Any non-Nebula, non-allowlisted process holding $\ge 2\text{ GB}$ (2048 MiB) of VRAM correlated with active GPU compute/3D utilization (`gpu_util_pct > 15%`) or foreground presence.
- **FR2 (Desktop & Compositor Allowlist):** Prevent false positives from normal desktop applications (Firefox, Discord, Cursor, VS Code, Chrome, Explorer) and Windows Desktop Window Manager (`dwm.exe`, holding 2.5–3.0 GB VRAM on 4K multi-monitor setups) even if maximized, F11 fullscreened, or using hardware acceleration VRAM.
- **FR3 (Task Pause):** Active autonomous tasks cleanly checkpoint or finish the current step and pause before executing further model turns.
- **FR4 (Model Unload):** Once paused, unload the model server (`llama-server`) to release all VRAM, entering `ModelState::Unloaded`.
- **FR5 (Auto-Resume):** After the game exits, wait for a cooldown (10 seconds) with no game re-detection, reload the model profile, and resume paused tasks.
- **FR6 (Manual Override):** Support `nebula game-mode on|off|status` and `nebula resume` to manually trigger or cancel game mode.
- **FR7 (Telemetry & Events):** Emit structured telemetry events (`game_mode.entered`, `game_mode.exited`) and stream `Event::GameModeChanged` on the IPC pipe.

