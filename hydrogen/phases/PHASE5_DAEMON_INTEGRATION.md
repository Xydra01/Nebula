# Project Hydrogen — Phase 5 Implementation Plan: Daemon Integration & Evaluation

| Field | Value |
| :--- | :--- |
| Status | Ready to plan |
| Component | Hydrogen Inference Engine & Nebula Daemon |
| Architecture Ref | [`docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md`](../../docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md) (Sections 3.3, 5, 6) |
| Dependencies | Phase 4 completed |
| GitHub Tag | `[hydrogen]`, label `hydrogen`, `phase-5` |

---

## 1. Goal

By the end of **Phase 5**:
- A zero-copy Win32 Named Shared Memory IPC ring buffer (`Local\Nebula_Hydrogen_IPC`) replaces TCP HTTP SSE, streaming prompts, tool descriptors, and tokens between the Nebula daemon and the Hydrogen engine in **$< 50\ \mu\text{s}$**.
- A safe Rust FFI wrapper (`crates/nebula-hydrogen`) implements the `ModelBackend` trait in `crates/nebula-model`, allowing Nebula to seamlessly swap between `llama-server` and `Hydrogen`.
- The immediate process spawn hook in `task_job_seam.rs` coordinates `active_tool_processes` using release-acquire memory barriers (`std::memory_order_release` in daemon, `std::memory_order_acquire` in engine), immediately switching CUDA scheduling to `cudaDeviceScheduleYield` when tool processes spawn.
- Hydrogen is verified against Nebula's full regression test suite and supercharges post-Phase 1 recursive self-improvement (Harness²), running 8 candidate probe rollouts in parallel in **$< 40\text{ seconds}$**.

---

## 2. In Scope & Out of Scope

### 2.1 In Scope
- **Win32 Named Shared Memory IPC**: $64\text{ MiB}$ ring buffer created via `CreateFileMappingW` / `MapViewOfFile` with Win32 synchronization events (`SetEvent`).
- **Rust FFI C-ABI Wrapper**: `crates/nebula-hydrogen` exposing safe async Rust bindings for Tokio.
- **ModelBackend Integration**: Implementing `ModelBackend` trait in `crates/nebula-model` with health checks, context management, and graceful stop.
- **Immediate Process Spawn Hook**: Shared memory atomic counter in `task_job_seam.rs` asserting tool process presence with `release` ordering, paired with `acquire` load in engine.
- **End-to-End Evaluation**: Running the regression benchmark (`bench/`) and Level 1 task recursion probes under Harness².

### 2.2 Out of Scope
- Remote multi-machine distributed clustering (post-Phase 1).

---

## 3. Workstreams & Issues Breakdown

| Issue ID | Title | Summary & Deliverable | Exit Check |
| :--- | :--- | :--- | :--- |
| **H16** | Win32 Named Shared Memory IPC Protocol | Implement 64 MiB ring buffer with flat binary serialization and Win32 event notification. | IPC benchmark demonstrates $<50\ \mu\text{s}$ token signaling latency. |
| **H17** | Rust FFI Wrapper & ModelBackend Implementation | Create `crates/nebula-hydrogen`, implement `ModelBackend` trait, and connect process spawn hook with acquire/release barriers. | `cargo test -p nebula-model` and integration tests pass with Hydrogen backend. |
| **H18** | End-to-End Regression & Harness² Probe Evaluation | Execute full multi-turn agent tasks with tools; run 8 concurrent Harness² candidate probes in parallel. | Completes 8 probe rollouts in $<40\text{ seconds}$ with zero memory leaks and 100% test pass. |

---

## 4. Phase 5 Exit Criteria

Phase 5 is considered **finished** only when all of the following verifiable conditions are met:

1. **Daemon Integration**: `nebula daemon start` successfully initializes the Hydrogen engine backend via shared memory, reporting health status `ok` in `nebula doctor`.
2. **IPC Throughput**: Inter-process token delivery latency across the Win32 ring buffer is strictly $<50\ \mu\text{s}$ (a $90\times$ reduction vs localhost TCP HTTP).
3. **CPU Contention Guard**: When a tool spawns in a Windows Job Object, the engine switches to `cudaDeviceScheduleYield` on the very next token iteration ($<14\text{ ms}$), preserving CPU cores for compilers.
4. **Harness² Acceleration**: Evaluating 8 candidate probes simultaneously under Level 1 task recursion completes in strictly $<40\text{ seconds}$ (matching the $35\text{ s}$ roofline in Section 5.2).
5. **Regression Parity**: All existing Phase 0 and Phase 1 test suites pass with zero regressions.
