# Project Hydrogen — Phase 0 Implementation Plan: Foundations & Harness

| Field | Value |
| :--- | :--- |
| Status | Ready to start |
| Component | Hydrogen Inference Engine |
| Target Machine | NVIDIA GeForce RTX 4070 12 GB, 32 GB RAM, Windows 11 (MSVC MSVC v143 / CUDA 12.x) |
| Architecture Ref | [`docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md`](../../docs/HYDROGEN_INFERENCE_ENGINE_DESIGN.md) (Sections 1, 2, 6) |
| GitHub Tag | `[hydrogen]`, label `hydrogen`, `phase-0` |

---

## 1. Goal

By the end of **Phase 0**:
- A reproducible, high-performance CMake/MSVC build harness is established in `hydrogen/` targeting Ada Lovelace SM 8.9 compute architecture (`sm_89`).
- A lightweight, modern C++20 GGUF parser (forked from ggml) is integrated to parse tensor metadata and quantized weights directly from existing `.gguf` files in `F:\Nebula\models\`.
- A minimal single-token forward smoke test loads the 9B Q4_K_M model onto the RTX 4070, verifies memory mapping, and computes a baseline forward step with verified logit output.
- Memory leak detection and GPU baseline telemetry are established before complex kernel optimizations begin.

---

## 2. In Scope & Out of Scope

### 2.1 In Scope
- **Toolchain Setup**: CMake 3.28+ with MSVC 2022 (`cl.exe`) and CUDA Toolkit 12.x (`nvcc`). Flags: `/O2 /std:c++20`, `nvcc` flags: `-O3 -gencode arch=compute_89,code=sm_89 --expt-relaxed-constexpr`.
- **GGUF Parser**: Header-only C++20 parser extracting tensor names, shapes, quant types (`Q4_K_M`, `Q8_0`), and metadata key-value pairs without external dependencies.
- **VRAM Weight Loader**: High-speed memory-mapped file loading (`CreateFileMappingW`) streaming model weights directly into device VRAM via `cudaMemcpyAsync`.
- **Smoke Test Executable**: `hydrogen_smoke_test.exe` verifying tensor layout and running a single forward embedding + output projection check.

### 2.2 Out of Scope (Phase 1+)
- Paged KV cache allocation and Radix tree branching (Phase 1).
- FlashInfer Cascade Attention and Marlin GEMM kernels (Phase 2).
- Continuous batching and GPU FSM grammars (Phase 3).
- Host DMA swapping and IPC shared memory (Phases 4 & 5).

---

## 3. Workstreams & Issues Breakdown

| Issue ID | Title | Summary & Deliverable | Exit Check |
| :--- | :--- | :--- | :--- |
| **H01** | Toolchain & SM 8.9 CUDA Harness | CMake configuration, MSVC flags, CUDA 12.x compiler integration, and basic vector-add sanity test. | `hydrogen_sanity_test.exe` runs on SM 8.9 device with exit code 0. |
| **H02** | GGUF Parser & Weight Loader | C++20 parser reading Qwen 2.5 9B Q4_K_M header, mapping tensor pointers into pinned host/VRAM. | Prints model architecture, tensor count, and verifies header checksums. |
| **H03** | Forward Smoke Test & Profiling | Single-token forward embedding pass, VRAM allocation profiling, and basic CUDA event timing harness. | Matches reference logit top-5 tokens; baseline latency recorded. |

---

## 4. Phase 0 Exit Criteria

Phase 0 is considered **finished** only when all of the following verifiable conditions are met:

1. **Compilation**: `cmake --build build --config Release` builds `hydrogen_smoke_test.exe` with zero compiler warnings under MSVC `/W4`.
2. **Model Ingestion**: The binary loads `qwen2.5-coder-7b-instruct-q4_k_m.gguf` (or 9B equivalent) in $<1.5\text{ seconds}$ using memory-mapped I/O, consuming exactly $5,203.5\text{ MiB} \pm 2\%$ VRAM as calculated in Proof A.
3. **Execution Correctness**: Running a 1-token forward pass produces identical top-5 token logits compared against `llama-cli` on the same prompt.
4. **Memory Hygiene**: Running under NVIDIA Nsight Systems or `cudaMemGetInfo` confirms zero VRAM memory leaks upon process exit.
