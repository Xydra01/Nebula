# Issue Plan: H02 — Modern C++20 GGUF Parser & Memory-Mapped Tensor Loader

| Field | Value |
| :--- | :--- |
| Issue ID | H02 |
| Title | `[hydrogen] feat(engine): Modern C++20 GGUF parser and memory-mapped tensor loader` |
| Phase | Phase 0: Foundations |
| Labels | `hydrogen`, `phase-0`, `core` |
| Target Files | `hydrogen/include/gguf_parser.hpp`, `hydrogen/src/model/tensor_loader.cpp` |

---

## 1. Problem Statement
To load existing `.gguf` quantized models (specifically Qwen 2.5 9B Q4_K_M) without pulling in the entire legacy `ggml` codebase, Hydrogen needs a lightweight, zero-dependency C++20 GGUF parser. The loader must parse model architecture metadata (head dimensions, GQA ratios, layer counts) and memory-map weight tensors via Win32 `CreateFileMappingW` for fast VRAM transfer.

## 2. Implementation Plan
1. Create `hydrogen/include/gguf_parser.hpp` implementing a modern struct-based parser reading GGUF v3 magic (`GGUF`), version, tensor count, and key-value metadata.
2. Implement typed extraction for GGUF metadata values (strings, integers, arrays).
3. Implement `hydrogen/src/model/tensor_loader.cpp` using Win32 memory-mapped files (`CreateFileMappingW`, `MapViewOfFile`) to stream tensor buffers into GPU device memory via `cudaMemcpyAsync`.
4. Validate tensor offsets and quantization tags (`GGML_TYPE_Q4_K`, `GGML_TYPE_Q8_0`).

## 3. Verification & Proof Plan
- Write a diagnostic runner `hydrogen_inspect_model.exe`.
- Run against `F:\Nebula\models\qwen2.5-coder-7b-instruct-q4_k_m.gguf` (or current active pin in `models.lock.toml`).
- Output must verify:
  - Architecture: `qwen2`
  - Layer count: `28` (or model pin equivalent)
  - Head dimension: `128`
  - Total mapped size: Matches file size on disk ($5.456\text{ GB} \pm 1\%$).

## 4. Acceptance Criteria
- [ ] GGUF parser compiles with zero warnings under C++20.
- [ ] Tensor loader parses all tensor metadata headers in $<100\text{ ms}$.
- [ ] Memory-mapping works on hot drive `F:` without copying full weights into intermediate heap buffers.
