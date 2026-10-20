# Issue Plan: H02 — Modern C++20 GGUF Parser & Memory-Mapped Tensor Loader

| Field | Value |
| :--- | :--- |
| Issue ID | H02 (GitHub Issue [#85](https://github.com/Xydra01/Nebula-Hydrogen/issues/85)) |
| Title | `[hydrogen] feat(engine): Modern C++20 GGUF parser and memory-mapped tensor loader` |
| Phase | Phase 0: Foundations |
| Target Branch | `feat/hydrogen-h02-gguf-loader` |
| Labels | `hydrogen`, `phase-0`, `core` |
| Target Files | `hydrogen/include/mapped_file.hpp`<br>`hydrogen/include/gguf_parser.hpp`<br>`hydrogen/include/tensor_loader.hpp`<br>`hydrogen/src/model/tensor_loader.cpp`<br>`hydrogen/src/tools/inspect_model.cpp`<br>`hydrogen/tests/test_h02_gguf.py` |

---

## 1. Problem Statement & Architecture

To load quantized transformer models without dependencies on the legacy `ggml` or `llama.cpp` codebases, **Project Hydrogen** requires a standalone, high-performance C++20 GGUF parser and zero-copy memory-mapped tensor loader.

Key architectural requirements:
1. **Zero-Copy Memory-Mapped I/O**: Multi-gigabyte GGUF models are mapped into the process's 64-bit virtual address space via Win32 `CreateFileMappingW` and `MapViewOfFile` with `FILE_FLAG_SEQUENTIAL_SCAN`. No weight bytes are copied into intermediate host heap buffers.
2. **GGUF v3 Binary Compliance**: Strictly parses GGUF magic (`0x46554747`), version, 64-bit tensor counts, and typed key-value metadata arrays (strings, integers, floats, nested arrays).
3. **Tensor Geometry & Offset Resolution**: Resolves tensor dimensions, quantization tags (`F32`, `F16`, `BF16`, `Q8_0`, `Q4_K`, `TQ1_0`), and computes direct byte pointers into the mapped file view.
4. **Sub-100ms Parsing Latency**: Traverses and indexes thousands of tensor headers and metadata entries in $<30\text{ ms}$.

---

## 2. Implementation Touchpoints

### 2.1 `hydrogen/include/mapped_file.hpp`
- RAII wrapper encapsulating Win32 `CreateFileW`, `CreateFileMappingW`, and `MapViewOfFile`.
- Automatically queries file size via `GetFileSizeEx` and provides constant-time access to the mapped byte pointer (`data()`).
- Unmaps view (`UnmapViewOfFile`) and closes handles (`CloseHandle`) deterministically upon destruction.

### 2.2 `hydrogen/include/gguf_parser.hpp`
- Defines `GGUFType` and `GGMLType` enumerations with mapping functions (`ggml_type_name`, `ggml_type_traits`).
- Struct-based parser providing typed accessors (`as_string()`, `as_uint32()`, `as_uint64()`, `as_float()`, `as_bool()`).
- Model representation (`GGUFModel`) indexing tensors by name and shape with direct pointers into the mapped view.

### 2.3 `hydrogen/src/model/tensor_loader.cpp`
- Implements `parse_gguf` using bounds-checked `BinaryReader` with unaligned memory safety (`std::memcpy`).
- Computes `tensor_data_offset` with 32-byte alignment (`general.alignment`).
- Calculates byte sizes based on quantization block dimensions (e.g., 32 elements/34 bytes for Q8_0, 256 elements/144 bytes for Q4_K).

### 2.4 `hydrogen/src/tools/inspect_model.cpp`
- Diagnostic CLI executable `hydrogen_inspect_model.exe <path_to_gguf> [--json]`.
- Prints human-readable architecture tables or structured JSON telemetry containing layer counts, head counts, context lengths, and quantization breakdowns.

### 2.5 `hydrogen/tests/test_h02_gguf.py`
- Pytest regression test suite executing against real model artifacts in `F:\Nebula\models\`:
  - `Ternary-Bonsai-2-27B-PTQ1_0-kv-bias.gguf` (66 KB, 16 tensors)
  - `Qwen3-Embedding-0.6B-Q8_0.gguf` (639 MB, 310 tensors)
  - `Ternary-Bonsai-2-27B-PTQ1_0.gguf` (5.94 GB, 851 tensors)
- Asserts parsing duration $< 100\text{ ms}$, tensor count consistency, and error handling for missing files.

---

## 3. Empirical Verification Results

```
============================= test session starts =============================
platform win32 -- Python 3.12.11, pytest-9.1.1, pluggy-1.6.0
rootdir: F:\Users\myfri\Project Neutron
plugins: anyio-4.15.1
collected 5 items

..\hydrogen\tests\test_h01_sanity.py::test_hydrogen_sm89_sanity_check PASSED [ 20%]
..\hydrogen\tests\test_h02_gguf.py::test_h02_inspect_kv_bias_model PASSED [ 40%]
..\hydrogen\tests\test_h02_gguf.py::test_h02_inspect_qwen_embedding_model PASSED [ 60%]
..\hydrogen\tests\test_h02_gguf.py::test_h02_inspect_bonsai_27b_model PASSED [ 80%]
..\hydrogen\tests\test_h02_gguf.py::test_h02_invalid_file_handling PASSED [100%]

============================== 5 passed in 3.18s ==============================
```

Sample JSON telemetry on 5.94 GB model (`Ternary-Bonsai-2-27B-PTQ1_0.gguf`):
- Parse duration: **24.581 ms** (target: $< 100\text{ ms}$)
- Total tensors: 851 (96 BF16, 353 F32, 402 TQ1_0)
- Architecture: `qwen35`, 64 layers, 262,144 context length, 5,120 embedding length

---

## 4. Acceptance Criteria

- [x] GGUF parser compiles with zero warnings under MSVC `/W4` and C++20.
- [x] Tensor loader parses all tensor metadata headers in $<100\text{ ms}$ (actual: 24.58 ms on 5.94 GB model).
- [x] Memory-mapping works on hot drive `F:` without copying weights into intermediate heap buffers.
- [x] Automated pytest test suite passes 100% cleanly.

