# Issue Plan: H04 — Uniform Page Block Layout & Dual-Quantization Tagging

| Field | Value |
| :--- | :--- |
| Issue ID | H04 |
| Title | `[hydrogen] feat(memory): Uniform page block container and dual FP8/Q8_0 quantization tagging` |
| Phase | Phase 1: Memory Subsystem |
| Labels | `hydrogen`, `phase-1`, `core` |
| Target Files | `hydrogen/include/page_block.hpp`, `hydrogen/src/memory/page_allocator.cpp` |

---

## 1. Problem Statement
Naively storing heterogeneous page sizes for FP8 ($80\text{ KiB/tok}$) and FP16 ($160\text{ KiB/tok}$) causes severe heap fragmentation. Furthermore, naive FP8 across deep GQA layers causes attention outlier clipping and code syntax drift. We need a uniform 1,280-byte `UniformPageBlock` (16 tokens) container that encapsulates both FP8 (E4M3) and Q8_0 quantizations with 1-byte tagging and dynamic scaling.

## 2. Implementation Plan
1. Define `struct UniformPageBlock` in `hydrogen/include/page_block.hpp` with 1-byte `quant_type` (`0=FP8_E4M3`, `1=Q8_0`), alignment padding, `dynamic_scale`, and 1,280-byte data payload.
2. Implement quantizer routines converting native FP16/BF16 activations into FP8 and Q8_0 blocks.
3. Configure layer-specific quantization policy: layers 0–3 and 24–27 automatically use Q8_0; layers 4–23 use FP8.

## 3. Verification & Proof Plan
- Write `test_page_block.cpp` validating `sizeof(UniformPageBlock) == 1288` (or 1,296 with 64-byte alignment).
- Run quantization round-trip test on token sequences:
  - Verify Q8_0 layers achieve $<0.02$ PPL delta vs native FP16.
  - Verify 16-token page serialization and deserialization.

## 4. Acceptance Criteria
- [ ] Memory footprint per 16 tokens is strictly uniform across all layers.
- [ ] Code syntax tokens (`{`, `}`, `;`, indentation) show zero degradation under Q8_0 sensitive layer fallback.
