# Issue Plan: H21 — Multi-Stream IPC Demuxer & Concurrent Router

| Field | Value |
| :--- | :--- |
| Issue ID | H21 |
| Title | `[hydrogen] fix(ipc): Centralized response ring demuxer router and concurrent multi-stream flow control` |
| Phase | Phase 6: Native Engine & Autonomous Agent Execution |
| Labels | `hydrogen`, `phase-6`, `core`, `ipc` |
| Target Files | `crates/nebula-hydrogen/src/ipc.rs`, `crates/nebula-model/src/hydrogen.rs`, `hydrogen/src/ipc/shm_server.cpp` |

---

## 1. Problem Statement

In [`crates/nebula-model/src/hydrogen.rs`](file:///f:/Users/myfri/Project%20Neutron/crates/nebula-model/src/hydrogen.rs#L157-L163), each active `chat()` call spawns an independent `pump_responses` task reading from the shared 48 MiB response ring buffer. When a frame arrives for a different `stream_id`, the task executes:
```rust
if frame.stream_id != stream_id {
    let _ = shm.read_response_payload(frame.payload_size as usize);
    continue;
}
```
This causes the first active stream reader to read and permanently **discard** the token frames of all other concurrent streams. Concurrent multi-stream inference (such as running 8 parallel sub-agents or Level 1 recursion candidate probes) is completely broken at the IPC layer.

## 2. Implementation Plan

1. **Centralized Background Demuxer Task in `HydrogenBackend`**:
   - Replace per-stream ring buffer readers with a single, dedicated background router task spawned when `HydrogenBackend` is created.
   - Maintain an internal concurrent routing table: `Arc<Mutex<HashMap<u32, mpsc::Sender<Result<StreamItem, ModelError>>>>>`.
   - The centralized router loops on `wait_engine_event()`, reads each incoming frame, inspects `frame.stream_id`, and immediately routes the frame/payload to the corresponding stream channel without reading ahead or discarding unassigned frames.
2. **Stream Registration & Teardown**:
   - When `HydrogenBackend::chat()` allocates a `stream_id`, it registers its channel sender in the routing table.
   - When a stream receives `FrameFlags::END_OF_STREAM` or is cancelled, its channel is deregistered, preventing memory leaks in the router table.
3. **Engine-Side Multi-Stream Interleaving in `shm_server.cpp`**:
   - Update `ShmServer` to ensure thread-safe concurrent writes across multiple engine worker streams using atomic sequence numbering and release-acquire ring head updates.
   - Ensure `send_token` and `send_response` preserve frame header integrity when called from parallel forward step iterations.

## 3. Verification & Proof Plan

- Write an integration test in `crates/nebula-hydrogen/tests/concurrent_shm_test.rs`:
  - Launch 8 parallel tokio tasks sending concurrent chat requests with stream IDs $1 \dots 8$.
  - Stream 10,000 interleaved tokens across all 8 streams simultaneously.
  - Verify every stream receives 100% of its expected tokens in exact sequence with zero dropped frames, zero timeouts, and zero cross-stream contamination.

## 4. Acceptance Criteria

- [ ] Single background demuxer task handles all response queue frame reads.
- [ ] Concurrent streams never discard or consume each other's frames.
- [ ] 8 concurrent streams operate smoothly over Win32 shared memory IPC with latency $< 50\ \mu\text{s}$.
- [ ] Clean channel deregistration on stream termination with zero channel leaks.
