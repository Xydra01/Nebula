# Issue Plan: H22 — Batched Continuous Scheduler & Paged Radix KV

| Field | Value |
| :--- | :--- |
| Issue ID | H22 |
| Title | `[hydrogen] feat(batcher): Multi-stream continuous decoding with stream-isolated SSM state and paged Radix KV deduplication` |
| Phase | Phase 6: Native Engine & Autonomous Agent Execution |
| Labels | `hydrogen`, `phase-6`, `core`, `memory`, `cuda` |
| Target Files | `hydrogen/include/batcher.hpp`, `hydrogen/include/radix_tree.hpp`, `hydrogen/include/cascade_attention.hpp`, `hydrogen/include/native_decoder.hpp`, `hydrogen/src/model/native_decoder.cu`, `hydrogen/src/batcher/split_fuse.cpp` |

---

## 1. Problem Statement

To run $B=8$ agent streams concurrently on an RTX 4070 (12 GB VRAM), the engine must overcome three fundamental architectural barriers:
1. **SSM State Bleed**: Ternary Bonsai-2 27B contains 48 State Space Model (SSM) layers. If the recurrent state buffer (`d_ssm_state_`) is not segmented and isolated per stream, the internal hidden state of one agent will contaminate other agents, destroying reasoning accuracy.
2. **VRAM Exhaustion Without Radix Deduplication**: Static model weights require 5.95 GB. Allocating unshared 8K KV context across 8 streams requires $> 6\text{ GB}$ of KV cache alone, causing an instant Out-Of-Memory (OOM) crash.
3. **WDDM TDR Timeouts on Large Prefills**: When an agent receives a large tool observation ($>2,000$ tokens), processing the entire prefill at once monopolizes Tensor Cores for $>250\text{ ms}$, causing active decode streams to freeze and risking Windows WDDM 2,000 ms driver resets.

## 2. Implementation Plan

1. **Stream-Isolated SSM Recurrent State Matrix**:
   - Allocate `d_ssm_state_` as a segmented 3D tensor:
     $$\text{Size} = B_{\text{max}} \times 48\text{ layers} \times \text{ssm\_inner} \times 4\text{ bytes}$$
     For $B_{\text{max}} = 8$ and $\text{ssm\_inner} = 6,144$:
     $$\text{Total Memory} = 8 \times 48 \times 6,144 \times 4 = 9,437,184\text{ bytes} \approx \mathbf{9.0\text{ MiB}}$$
   - In `kernel_ssm_step`, offset the state pointer strictly by `stream_slot * (48 * ssm_inner)` to guarantee absolute memory isolation across parallel agents.
2. **Paged Radix-Tree KV Deduplication**:
   - Replace the single contiguous `d_k_cache_` / `d_v_cache_` pointers in `NativeDecoder` with physical 16-token page tables managed by `RadixTree` (Issue H05) and `PagePool` (Issue H06).
   - The shared workspace prefix (system prompt, guidelines, repo context: $\sim 4,096$ tokens) is stored once in the radix root.
   - Each of the 8 agent streams branches from the shared root with private Copy-on-Write leaf pages, capping total KV memory to $< 1.7\text{ GB}$ (only $28\%$ of available dynamic pool).
3. **Batched Cascade Attention Decoding (SM 8.9)**:
   - Integrate `CascadeAttention` (Issue H07) into the forward pass:
     - Stage 1: Shared prefix attention executed once via GEMM across the common radix root.
     - Stage 2: Paged leaf attention executed across private agent stream pages in parallel.
4. **Split-Fuse 64-Token Prefill Clamping (WDDM TDR Guard)**:
   - Incoming tool observations are queued in `SplitFuseBatcher` (Issue H10).
   - In each continuous forward pass, clamp the prefill slice to strictly $M \le 64$ tokens ($t_{\text{compute}} \approx 9.76\text{ ms} < 14.08\text{ ms}$).
   - Piggyback prefill chunks onto active decode steps with $< 10\%$ latency jitter and zero perceptible UI freeze.

## 3. Verification & Proof Plan

- Run `hydrogen_bench_split_fuse_batcher --streams 8 --prefill 2000 --json`.
- Verify total VRAM footprint remains strictly below $3.5\text{ GB}$ of KV cache during 8-stream parallel execution.
- Verify per-stream decode step latency is bounded by $\le 15.5\text{ ms}$, with aggregate throughput $\ge 500\text{ tokens/s}$.
- Verify zero SSM cross-talk: run two divergent mathematical prompts on streams 1 and 2 simultaneously; verify output streams match independent serial runs with 100% bit-exact token equivalence.

## 4. Acceptance Criteria

- [ ] SSM state vectors are strictly isolated per stream ID with zero state contamination.
- [ ] Radix-Tree prefix deduplication saves $\ge 60\%$ of KV cache memory during 8-stream execution.
- [ ] Split-Fuse batcher clamps all prefill chunks to $M \le 64$ tokens, preventing WDDM driver timeouts.
- [ ] Continuous batching achieves $\ge 500\text{ tokens/s}$ aggregate throughput on RTX 4070.
