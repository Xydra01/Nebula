# Issue Plan: H17 — Rust FFI Bridge & Nebula ModelBackend Implementation

| Field | Value |
| :--- | :--- |
| Issue ID | H17 |
| Title | `[hydrogen] feat(daemon): Rust FFI bridge and ModelBackend implementation with memory ordering barriers` |
| Phase | Phase 5: Daemon Integration |
| Labels | `hydrogen`, `phase-5`, `core` |
| Target Files | `crates/nebula-hydrogen/src/lib.rs`, `crates/nebula-model/src/hydrogen.rs` |

---

## 1. Problem Statement
The Nebula daemon supervises local models through the `ModelBackend` trait defined in `crates/nebula-model/src/backend.rs`. To replace `llama-server` transparently, Hydrogen must expose a safe Rust FFI crate (`crates/nebula-hydrogen`) implementing `ModelBackend`. Furthermore, when the daemon launches child processes in Windows Job Objects, it must assert `active_tool_processes` across shared memory with `release` ordering, paired with an `acquire` barrier in the engine to immediately transition from `cudaDeviceScheduleSpin` to `Yield`.

## 2. Implementation Plan
1. Create new workspace crate `crates/nebula-hydrogen` containing safe C-ABI bindings to `hydrogen_ipc.h`.
2. Implement `ModelBackend` trait in `crates/nebula-model/src/hydrogen.rs`:
   - `start()`, `stop()`, `health()`, and `stream_chat()`.
3. In `crates/nebula-tools/src/builtins/task_job_seam.rs`, assert the process spawn counter across shared memory:
   ```cpp
   engine_ctrl->active_tool_processes.fetch_add(1, std::memory_order_release);
   ```
4. In the engine control loop, load this counter with `std::memory_order_acquire` before each batch dispatch boundary:
   ```cpp
   uint32_t active = engine_ctrl->active_tool_processes.load(std::memory_order_acquire);
   if (active > 0) set_schedule_mode(cudaDeviceScheduleYield);
   ```
5. Add `[model.backend]` configuration key in `crates/nebula-config/src/model.rs` and `config/default.toml` (`backend = "hydrogen"`).

## 3. Verification & Proof Plan
- Run `cargo test -p nebula-hydrogen --locked`.
- Run `cargo test -p nebula-model --locked`.
- Verify memory barrier correctness: test that spawning a tool process switches CUDA scheduling to `Yield` on the very next token iteration ($<14\text{ ms}$).

## 4. Acceptance Criteria
- [ ] `ModelBackend` trait passes all existing backend mock and integration tests.
- [ ] Safe Rust wrapper contains zero unsafe code outside documented `// SAFETY:` FFI blocks.
- [ ] Memory ordering barriers prevent MSVC `/O2` instruction reordering across kernel dispatches.
