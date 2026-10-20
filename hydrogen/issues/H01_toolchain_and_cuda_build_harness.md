# Issue Plan: H01 — Toolchain, MSVC Environment & SM 8.9 CUDA Build Harness

| Field | Value |
| :--- | :--- |
| Issue ID | H01 (GitHub Issue [#84](https://github.com/Xydra01/Nebula-Hydrogen/issues/84)) |
| Title | `[hydrogen] feat(engine): Toolchain, MSVC environment, and SM 8.9 CUDA build harness` |
| Phase | Phase 0: Foundations |
| Target Branch | `feat/hydrogen-h01-build-harness` |
| Labels | `hydrogen`, `phase-0`, `core` |
| Target Files | `hydrogen/CMakeLists.txt`<br>`hydrogen/src/sanity_check.cu`<br>`hydrogen/scripts/build.ps1`<br>`hydrogen/tests/test_h01_sanity.py` |

---

## 1. Problem Statement & Objectives

To achieve ultra-high inference throughput without dependencies on WSL2, Linux emulators, or heavy third-party runtimes, **Project Hydrogen** requires a standalone, reproducible native Windows build harness. 

The harness must:
1. Target the host's physical NVIDIA GeForce RTX 4070 Ada Lovelace architecture (`compute_89,sm_89`).
2. Integrate Microsoft Visual Studio 2022 MSVC (`cl.exe` v143 / 19.44) and NVIDIA CUDA Toolkit 13.2 (`nvcc.exe`).
3. Enforce the ISO C++20 standard (`/std:c++20`, `CMAKE_CUDA_STANDARD 20`) with strict warning hygiene.
4. Establish an automated build and sanity-test pipeline (`build.ps1` and `test_h01_sanity.py`) that executes in CI and local development without manual PATH manipulation.
5. Provide a memory-safe, lightweight GPU sanity-check executable (`hydrogen_sanity_test.exe`) that verifies hardware discovery, asynchronous CUDA stream execution, and zero VRAM leakage before larger model loading routines in H02/H03 are compiled.

---

## 2. Toolchain & Environment Specifications

The build harness interacts directly with the local machine toolchain discovered on drive `F:`:

| Component | Path / Identification | Configuration Parameter |
| :--- | :--- | :--- |
| **GPU Target** | NVIDIA GeForce RTX 4070 (WDDM 3.2, 12,282 MiB VRAM) | Compute Architecture: `sm_89`, `compute_89` |
| **CUDA Toolkit** | `F:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.2` | `CUDA_PATH`, `nvcc.exe` (v13.2.51) |
| **Host Compiler** | `F:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools` | `cl.exe` (19.44.35207, MSVC v143) |
| **Build System** | `F:\Program Files\CMake\bin\cmake.exe` (v4.3.2) | Generator: `Ninja` (`-G Ninja`) |
| **Build Output** | `hydrogen/build/` | Out-of-source build tree |

---

## 3. Implementation Touchpoints & File Blueprints

### 3.1 `hydrogen/CMakeLists.txt`
- Minimum CMake Version: `3.28`.
- Languages: `CXX` and `CUDA`.
- Architecture Target: `set(CMAKE_CUDA_ARCHITECTURES 89)`.
- Standards:
  - `set(CMAKE_CXX_STANDARD 20)`
  - `set(CMAKE_CXX_STANDARD_REQUIRED ON)`
  - `set(CMAKE_CUDA_STANDARD 20)`
  - `set(CMAKE_CUDA_STANDARD_REQUIRED ON)`
- MSVC Compile Options (CXX):
  - `/std:c++20 /permissive- /utf-8 /W4 /O2 /Oi /Gy`
  - Defines: `NOMINMAX`, `WIN32_LEAN_AND_MEAN`, `_CRT_SECURE_NO_WARNINGS`.
- NVCC Flags (CUDA):
  - `-gencode arch=compute_89,code=sm_89`
  - `-O3 --use_fast_math --expt-relaxed-constexpr`
  - Forward MSVC C++20 flags via `-Xcompiler`: `-Xcompiler="/std:c++20,/permissive-,/utf-8,/W4"`
- Executable Target:
  - `add_executable(hydrogen_sanity_test src/sanity_check.cu)`
  - `target_include_directories(hydrogen_sanity_test PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include ${CMAKE_CURRENT_SOURCE_DIR}/src)`
  - Runtime linking: CUDA Runtime statically or dynamically linked (`cudart`).

### 3.2 `hydrogen/src/sanity_check.cu`
The sanity test executable provides rigorous, empirical proof of device execution:
1. **Device Discovery**:
   - Calls `cudaGetDeviceProperties(&prop, 0)`.
   - Asserts compute capability `prop.major == 8 && prop.minor == 9`.
   - Records device name, multiprocessor count (46 SMs), warp size (32), and total global memory (~12 GB).
2. **Memory Baseline & Leak Checking**:
   - Records baseline free/total VRAM via `cudaMemGetInfo(&mem_free_pre, &mem_total)`.
   - Enforces a safety allocation ceiling: $N = 1,048,576$ elements ($1\text{ M}$ `float`s = $4\text{ MB}$ per buffer, $12\text{ MB}$ total VRAM footprint across inputs $A, B$ and output $C$).
   - Pinned host buffers allocated via `cudaMallocHost` for asynchronous page-locked DMA transfers.
3. **Asynchronous Vector-Add Kernel Execution**:
   - `__global__ void vector_add_kernel(const float* __restrict__ a, const float* __restrict__ b, float* __restrict__ c, size_t n)`
   - Kernel launch configuration: 4,096 thread blocks with 256 threads per block ($4096 \times 256 = 1,048,576$ threads).
   - Launched onto a non-default stream (`cudaStreamCreateWithFlags(&stream, cudaStreamNonBlocking)`).
   - CUDA event timing: records launch-to-completion duration via `cudaEventRecord`.
4. **Result Verification**:
   - DMA copy-back: `cudaMemcpyAsync(h_c, d_c, bytes, cudaMemcpyDeviceToHost, stream)`.
   - `cudaStreamSynchronize(stream)`.
   - Element-wise CPU validation: iterates over all 1,048,576 elements; verifies $|c[i] - (a[i] + b[i])| < 10^{-6}$.
5. **Memory Teardown & Post-Check**:
   - Frees device pointers (`cudaFree`) and host pointers (`cudaFreeHost`).
   - Destroys events and stream.
   - Queries `cudaMemGetInfo(&mem_free_post, &mem_total)`.
   - Asserts zero uncollected device allocations (`mem_free_post == mem_free_pre`).
6. **Structured Telemetry Output**:
   - Outputs machine-parsable JSON to stdout:
     ```json
     {
       "status": "PASS",
       "device_name": "NVIDIA GeForce RTX 4070",
       "compute_capability": "8.9",
       "multiprocessors": 46,
       "total_vram_bytes": 12878643200,
       "verified_elements": 1048576,
       "kernel_duration_us": 42.1,
       "vram_leak_bytes": 0
     }
     ```

### 3.3 `hydrogen/scripts/build.ps1`
A robust PowerShell build script automating environment configuration and compilation:
1. Discovers MSVC installation via `F:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe` for component `Microsoft.VisualStudio.Component.VC.Tools.x86.x64`.
2. Locates and executes `vcvars64.bat` in a temporary `cmd.exe` subshell to extract environment variables (`PATH`, `INCLUDE`, `LIB`, `LIBPATH`) without polluting global system state.
3. Injects extracted environment variables into the running PowerShell process.
4. Verifies `cmake` and `ninja` are executable.
5. Configures CMake out-of-source: `cmake -G Ninja -B build -S . -DCMAKE_BUILD_TYPE=Release`.
6. Compiles target: `cmake --build build --config Release`.
7. Optional `-Run` switch: automatically executes `build/bin/hydrogen_sanity_test.exe` and reports return code.

### 3.4 `hydrogen/tests/test_h01_sanity.py`
Automated Python test harness integrated with `pytest`:
1. Executes `hydrogen/scripts/build.ps1 -Run`.
2. Asserts exit code `0`.
3. Parses stdout JSON telemetry.
4. Asserts:
   - `json_data["status"] == "PASS"`
   - `json_data["compute_capability"] == "8.9"`
   - `"RTX 4070" in json_data["device_name"]`
   - `json_data["verified_elements"] == 1048576`
   - `json_data["vram_leak_bytes"] == 0`

---

## 4. Invariants & Safety Constraints

1. **Hardware Safety**: Allocation size is strictly capped at $12\text{ MB}$ VRAM. The system currently hosts active `llama-server` processes occupying ~11.3 GB of 12.2 GB VRAM; an allocation $> 500\text{ MB}$ would risk `CUDA_ERROR_OUT_OF_MEMORY`.
2. **Drive Isolation**: In accordance with workspace rules, no files or compiler scratch caches may resolve to `C:`. Build outputs reside strictly on `F:\Users\myfri\Project Neutron\hydrogen\build\`.
3. **WDDM Execution Bounds**: The vector addition kernel executes in $< 100\ \mu\text{s}$, well within the sub-5ms WDDM preemption budget.

---

## 5. Verification & Acceptance Criteria

- [ ] `hydrogen/CMakeLists.txt` configures with Ninja and MSVC C++20 without errors or deprecation warnings.
- [ ] `hydrogen/src/sanity_check.cu` compiles cleanly with SM 8.9 architecture flags (`arch=compute_89,code=sm_89`).
- [ ] `hydrogen/scripts/build.ps1` runs non-interactively, detects `vcvars64.bat`, and compiles the Release binary.
- [ ] Executable runs on the physical RTX 4070, outputs valid JSON telemetry, validates all 1,048,576 elements, and records zero memory leaks.
- [ ] `pytest hydrogen/tests/test_h01_sanity.py` passes cleanly in $< 15\text{ seconds}$.
- [ ] Workspace CI checks (`cargo fmt`, `cargo clippy`, `cargo nextest`, Python `ruff`) remain 100% green.

