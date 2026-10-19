# Issue Plan: H16 — Win32 Named Shared Memory IPC Protocol & Event Signaling

| Field | Value |
| :--- | :--- |
| Issue ID | H16 |
| Title | `[hydrogen] feat(ipc): Win32 named shared memory ring buffer and zero-copy token signaling` |
| Phase | Phase 5: Daemon Integration |
| Labels | `hydrogen`, `phase-5`, `core` |
| Target Files | `hydrogen/src/ipc/shm_server.cpp`, `hydrogen/include/hydrogen_ipc.h` |

---

## 1. Problem Statement
The baseline `llama-server` architecture uses localhost TCP HTTP SSE (`http://127.0.0.1:8080/completion`). TCP loopback, HTTP header formatting, and JSON serialization introduce 4.5 ms of transport latency and 0.8 ms of deserialization latency on every tool call. Hydrogen replaces this with a zero-copy Win32 Named Shared Memory IPC interface, achieving $<50\ \mu\text{s}$ token signaling.

## 2. Implementation Plan
1. Define binary protocol layout in `hydrogen/include/hydrogen_ipc.h`: flat binary message structs for requests, tokens, tool descriptors, and completion signals.
2. Implement `shm_server.cpp`: create a $64\text{ MiB}$ named shared memory ring buffer at `Local\Nebula_Hydrogen_IPC` using `CreateFileMappingW` and `MapViewOfFile`.
3. Implement Win32 synchronization events (`CreateEventW`, `SetEvent`, `WaitForSingleObject`) for sub-microsecond notification without OS thread sleep penalties.
4. Provide non-blocking ring buffer read/write cursors using atomic indices (`std::atomic<uint32_t> head, tail`).

## 3. Verification & Proof Plan
- Write `bench_shm_ipc.cpp`.
- Benchmark round-trip token signaling latency between two separate Windows processes:
  - Verify message transmission and event signaling completes in strictly $<50\ \mu\text{s}$ (a $90\times$ improvement over 4.5 ms HTTP).
  - Stream 100,000 tokens through the ring buffer with zero lost or corrupted frames.

## 4. Acceptance Criteria
- [ ] Ring buffer operates zero-copy across process boundaries.
- [ ] Token signaling latency $<50\ \mu\text{s}$.
- [ ] Clean Win32 event cleanup and handle closing on process termination.
