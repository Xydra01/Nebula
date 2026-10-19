# Issue Plan: H01 — Toolchain, MSVC Environment & SM 8.9 CUDA Build Harness

| Field | Value |
| :--- | :--- |
| Issue ID | H01 |
| Title | `[hydrogen] feat(engine): Toolchain, MSVC environment, and SM 8.9 CUDA build harness` |
| Phase | Phase 0: Foundations |
| Labels | `hydrogen`, `phase-0`, `core` |
| Target Files | `hydrogen/CMakeLists.txt`, `hydrogen/src/sanity_check.cu` |

---

## 1. Problem Statement
To build an ultra-high-performance inference engine on Windows without dependencies on WSL or Linux, we must establish a standalone CMake build system integrated with MSVC 2022 (`cl.exe`) and CUDA Toolkit 12.x (`nvcc`). The build configuration must strictly target NVIDIA Ada Lovelace SM 8.9 (`compute_89,sm_89`), enforcing modern C++20 standards, high optimization levels (`/O2`, `-O3`), and strict warning hygiene (`/W4 /WX` equivalent).

## 2. Implementation Plan
1. Create `hydrogen/CMakeLists.txt` specifying `CMAKE_CUDA_ARCHITECTURES 89`.
2. Configure MSVC-specific compile options (`/std:c++20`, `/permissive-`, `/utf-8`, `/O2`).
3. Configure `nvcc` flags (`--expt-relaxed-constexpr`, `-O3`, `--use_fast_math`).
4. Write `hydrogen/src/sanity_check.cu` containing a lightweight vector-add test kernel running across multiple thread blocks to verify CUDA device discovery and execution.
5. Create a PowerShell build script `hydrogen/scripts/build.ps1` automating CMake generation and compilation with MSVC build tools.

## 3. Verification & Proof Plan
- Execute `powershell -NoProfile -File hydrogen/scripts/build.ps1`.
- Run `build/bin/Release/hydrogen_sanity_test.exe`.
- Verify output logs report:
  - GPU Device: `NVIDIA GeForce RTX 4070`
  - Compute Capability: `8.9`
  - Total Global Memory: `12282 MB` (or equivalent)
  - Sanity vector addition: `ALL 1,048,576 ELEMENTS VERIFIED`

## 4. Acceptance Criteria
- [ ] CMake configuration configures without warnings on Windows 11.
- [ ] Executable compiles cleanly under Release mode.
- [ ] CUDA SM 8.9 device properties match the host RTX 4070 hardware.
