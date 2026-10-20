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
- [x] Memory footprint per 16 tokens is strictly uniform across all layers (1,288 bytes = 8 bytes metadata + 1,280 bytes payload).
- [x] Code syntax tokens (`{`, `}`, `;`, indentation) show zero degradation under Q8_0 sensitive layer fallback (MSE: $3.91 \times 10^{-4}$, SNR: $34.85\text{ dB}$).

---

## 5. Verification Evidence & Telemetry Output

### 5.1 Binary Execution Telemetry (`hydrogen_test_page_block.exe`)
```
===================================================
Project Hydrogen: Uniform Page Block Layout Verification
===================================================
UniformPageBlock Size:     1288 bytes (Target: 1,288 bytes) - PASS
Memory Layout Offsets:
  quant_type:              Offset 0 (uint8_t tag)
  dynamic_scale:           Offset 4 (float)
  data payload:            Offset 8 (1,280 bytes)
Capacity per Page:         16 tokens @ 80 bytes/token
---------------------------------------------------
Dual-Quantization Hybrid Policy:
  28-Layer Geometry:       Layers 0..3: Q8_0 | 4..23: FP8 | 24..27: Q8_0
  64-Layer Geometry:       Layers 0..3: Q8_0 | 4..59: FP8 | 60..63: Q8_0
  Policy Verification:     PASS (Sensitive Boundaries Protected)
---------------------------------------------------
Quantization Round-Trip Fidelity:
  Q8_0 MSE:                3.914340e-04 (SNR: 34.85 dB)
  FP8 E4M3 MSE:            8.01e-04 (SNR: 31.74 dB)
  Code Syntax Outliers:    PASS (Zero syntax degradation)
===================================================
Overall Status:            PASS (All Invariants Verified)
```

### 5.2 Structured JSON Verification Output (`hydrogen_test_page_block.exe --json`)
```json
{
  "status": "PASS",
  "page_size_bytes": 1288,
  "page_payload_bytes": 1280,
  "tokens_per_page": 16,
  "bytes_per_token": 80,
  "size_invariant_verified": true,
  "layout_offsets_verified": true,
  "dual_quant_policy_verified": true,
  "syntax_tokens_preserved": true,
  "q8_0": {
    "mse": 0.000391,
    "max_error": 0.033463,
    "snr_db": 34.85
  },
  "fp8_e4m3": {
    "mse": 0.000801,
    "max_error": 0.278572,
    "snr_db": 31.74
  }
}
```

### 5.3 Automated Pytest Suite (`hydrogen/tests/test_h04_page_block.py`)
```
hydrogen/tests/test_h04_page_block.py::test_page_block_binary_exists PASSED [ 20%]
hydrogen/tests/test_h04_page_block.py::test_page_block_help PASSED          [ 40%]
hydrogen/tests/test_h04_page_block.py::test_page_block_execution_and_invariants PASSED [ 60%]
hydrogen/tests/test_h04_page_block.py::test_quantization_fidelity_bounds PASSED [ 80%]
hydrogen/tests/test_h04_page_block.py::test_page_block_formatted_output PASSED [100%]
============================== 5 passed in 0.08s ==============================
```

