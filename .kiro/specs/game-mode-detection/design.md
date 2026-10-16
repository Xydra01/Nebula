# Design: Game Mode Detection

## 1. Components
1. **Config (`nebula-config`):** `GameModeConfig` with `enabled`, `poll_interval_s`, `vram_threshold_mib`, `cooldown_secs`, `known_processes`, and `desktop_allowlist`.
2. **Win32 Layer (`nebula-daemon/src/win.rs`):** Safe isolated wrappers over Win32 foreground window and monitor APIs, abstracted behind `ForegroundDetector` trait seam.
3. **Coordinator (`nebula-daemon/src/game_mode.rs`):** Coordinates background polling, game detection, cooldown tracking, model unloading (`chat.unload()`), model re-provisioning (`chat.ensure_profile()`), and task pause/resume signaling via `watch::Sender<bool>`.
4. **Executor Integration (`nebula-daemon/src/executor.rs`):** Task steps call `shared.game_mode.wait_if_active(&ctx.token).await` between steps.
5. **IPC & CLI (`nebula-proto` & `nebula-cli`):**
   - Methods `Method::GameModeSet` and `Method::GameModeStatus`.
   - Event `Event::GameModeChanged`.
   - CLI commands `nebula game-mode on|off|status` and `nebula resume`.
