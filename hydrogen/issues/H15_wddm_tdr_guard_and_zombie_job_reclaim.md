# Issue Plan: H15 — 30s Zombie Process Auto-Reclaim & 500ms WDDM TDR Guard

| Field | Value |
| :--- | :--- |
| Issue ID | H15 |
| Title | `[hydrogen] feat(safety): 30s zombie process auto-reclaim, DMA quiescence, and 500ms WDDM TDR guard` |
| Phase | Phase 4: DMA Swapping & WDDM |
| Labels | `hydrogen`, `phase-4`, `safety` |
| Target Files | `hydrogen/src/memory/watermark_supervisor.cpp`, `hydrogen/src/memory/dma_guard.cpp` |

---

## 1. Problem Statement
If swapped memory hits the 3.5 GB high-water mark, incoming tool waits transition to `VRAM_RESIDENT_PAUSED`. If an active tool process hangs indefinitely in a Windows Job Object (infinite loop or deadlocked socket), paused streams remain frozen in VRAM, starving active decoders and permanently deadlocking the engine. Furthermore, if a hung DMA transfer causes `cudaStreamSynchronize` to block for $>2,000\text{ ms}$, Windows WDDM's Timeout Detection and Recovery (TDR) mechanism forcibly resets the GPU display driver, crashing the entire engine.

## 2. Implementation Plan
1. Implement a **3.5 GB high-water mark queue** (3,584 MiB / 87.5% of pool capacity), directing new tool waits to `VRAM_RESIDENT_PAUSED` rather than spilling to NVMe.
2. Enforce a **30-second hard timeout** on paused streams (tied to `limits.call_timeout_seconds` defaulting to 60s): if the high-water mark does not clear within 30 seconds, signal the daemon to call `TerminateJobObject` on the oldest delinquent process.
3. Enforce **In-Flight DMA Quiescence**: ensure hardware quiescence before clearing slab bitmask words.
4. Implement a **500 ms non-blocking WDDM TDR guard**: wrap DMA synchronization in a non-blocking `cudaStreamQuery` polling loop bounded by a 500 ms backstop:
   ```cpp
   auto start = std::chrono::steady_clock::now();
   while (cudaStreamQuery(swap_stream) == cudaErrorNotReady) {
       auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
           std::chrono::steady_clock::now() - start).count();
       if (elapsed > 500) { // 4x safety factor before Windows 2,000 ms TdrDelay
           log_error("PCIe DMA transfer hung (>500 ms); executing controlled engine restart");
           trigger_safe_engine_restart();
           return;
       }
       std::this_thread::sleep_for(std::chrono::microseconds(100));
   }
   ```

## 3. Verification & Proof Plan
- Write `test_deadlock_and_tdr.cpp`.
- Test 1 (Zombie Reclaim): Simulate 30 concurrent tool streams with 5 hung processes:
  - Verify that after 30 seconds, `TerminateJobObject` terminates the delinquent process tree.
  - Verify swapped host slots are safely reclaimed and paused VRAM streams unfreeze.
- Test 2 (TDR Guard): Simulate an artificial DMA hardware stall:
  - Verify `cudaStreamQuery` loop triggers orderly engine restart at 500 ms.
  - Verify Windows display driver does not trigger a TDR reset (`0x117` / `nvlddmkm.sys`).

## 4. Acceptance Criteria
- [ ] 30-second watermark timeout terminates zombie processes and reclaims host memory.
- [ ] In-flight DMA quiescence prevents write-after-free corruption.
- [ ] 500 ms backstop prevents Windows WDDM TDR driver resets with a 4x safety factor.
