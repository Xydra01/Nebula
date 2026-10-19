# Project Hydrogen — Phase 3 Implementation Plan: Batching & Tool Grammar

| Field | Value |
| :--- | :--- |
| Status | Ready to plan |
| Component | Hydrogen Inference Engine |
| Architecture Ref | [`docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md`](../../docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md) (Sections 2, 3.3, 3.4, 3.5) |
| Dependencies | Phase 2 completed |
| GitHub Tag | `[hydrogen]`, label `hydrogen`, `phase-3` |

---

## 1. Goal

By the end of **Phase 3**:
- The **Split-Fuse continuous batcher** dynamically interleaves $B$ active single-token decode requests with at most **64 tokens** from an incoming prefill queue, completely eliminating prefill-induced typing stutters.
- The **GPU-Fused FSM Bitmask Sampler** pre-compiles JSON tool schemas into 19 KB SRAM bitmasks, enforcing strict tool grammar directly inside the CUDA sampling kernel in **$1.5\ \mu\text{s}$** without CPU synchronization.
- The **Speculative Tool Dispatcher** starts asynchronous background I/O for read-only tools upon argument closing quotation marks, while enforcing an ironclad turn-finalization safety barrier on mutating operations (`fs.write`, `shell.run`).

---

## 2. In Scope & Out of Scope

### 2.1 In Scope
- **Split-Fuse Batcher**: Dynamic batch assembly clamping chunked prefills to $M \le 64$ tokens per step ($t_{\text{compute}} \le 9.76\text{ ms}$).
- **GPU-Fused FSM Bitmask Kernel**: Pre-compiled deterministic FSM state tables converting JSON schema to 19 KB bitmasks staged into CUDA Shared Memory (SRAM).
- **Speculative Read-Only Tool Dispatch**: Asynchronous I/O worker in child Job Object for idempotent tools (`fs.read`, `fs.list`, `git.status`).
- **Turn Finalization Seam & Safety Whitelist**: Strict barrier preventing speculative execution of Tier 1+ mutating commands (`fs.write`, `shell.run`, `git.commit`).

### 2.2 Out of Scope
- Swapping leaf KV memory to host RAM across PCIe (Phase 4).
- Win32 shared memory ring buffer IPC (Phase 5).

---

## 3. Workstreams & Issues Breakdown

| Issue ID | Title | Summary & Deliverable | Exit Check |
| :--- | :--- | :--- | :--- |
| **H10** | Split-Fuse Continuous Batching Scheduler | Implement mixed prefill/decode batching queue clamping prefill chunks strictly to 64 tokens per step. | Benchmark verifies that ingesting a 1,500-token prefill causes $\le 10\%$ latency jitter on active decode streams. |
| **H11** | GPU-Fused FSM Bitmask Sampler | Pre-compile tool schemas to state bitmask table; implement CUDA top-p/temperature kernel with SRAM bitmask masking. | Schema validation executes in $<2\ \mu\text{s}$ with 100% valid JSON syntax across 10,000 generated calls. |
| **H12** | Speculative Tool Parser & Mutating Barrier | Extract tool arguments during token emission; trigger async I/O on closing quote; enforce turn-finalization barrier on mutating tools. | Tool turn round-trip latency drops to $\le 35\text{ ms}$ ($\ge 60\%$ reduction) with 0 premature mutations. |

---

## 4. Phase 3 Exit Criteria

Phase 3 is considered **finished** only when all of the following verifiable conditions are met:

1. **Jitter-Free Prefill Ingestion**: While 8 decode streams are actively generating tokens, injecting a 1,500-token tool observation prefill results in $\le 10\%$ per-step latency variance (maximum decode step $\le 15.5\text{ ms}$, zero perceptible typing freeze).
2. **Grammar Kernel Performance**: GPU-fused FSM bitmask evaluation adds $<2.0\ \mu\text{s}$ to the top-p sampling kernel and maintains 100% JSON schema conformity with zero invalid JSON tokens emitted.
3. **Safety Invariant Verification**: Mutating tools (Tier 1+) never execute speculatively under any circumstance; execution occurs strictly after complete turn termination and explicit daemon approval.
4. **Latency Verification**: End-to-end tool-turn round-trip latency on `fs.read` drops from $99.4\text{ ms}$ (baseline `llama-server`) to strictly $\le 35.0\text{ ms}$ (matching Proof D).
