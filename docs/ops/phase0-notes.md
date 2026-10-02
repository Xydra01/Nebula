# Phase 0 Ops Notes

Running log of machine-prep results for [PHASE0_PLAN.md](../PHASE0_PLAN.md) WS1.

## 2026-10-01: WS1 admin pass (`scripts/phase0-ws1-admin.ps1`)

Log: `F:\Nebula\setup\ws1-admin.log`

### 1.1a Page file moved off `C:`

- `AutomaticManagedPagefile` disabled; non-`F:` page files removed.
- `F:\pagefile.sys`: initial 4096 MB, max 12288 MB.
- **Takes effect after reboot.** Verify with `Get-CimInstance Win32_PageFileUsage` (expect only `F:\pagefile.sys`).

### 1.13 smartmontools

- smartmontools 7.5 installed via winget to `%ProgramFiles%\smartmontools` (`F:\Program Files\...`).
- Snapshot script: `F:\Nebula\setup\smart-snapshot.ps1` (source: `scripts/smart-snapshot.ps1`), output `F:\Nebula\state\smart\`.
  - Intel RST exposes SATA drives twice (`/dev/sdX` and `/dev/csmi0,N`); the script dedupes by serial.
- Scheduled tasks (SYSTEM, highest privileges):
  - `Nebula\SMART monthly`: 1st of each month, 04:00.
  - `Nebula\SMART on disk error`: System log, provider `disk`, event IDs 7, 51, 153.

### 1.1b SMART baseline

| Drive | Letter | Health | Power-on hours | Temp | Key indicators |
|---|---|---|---|---|---|
| Samsung SSD 980 1TB (NVMe) | `F:` | PASSED | 21,737 | 41 °C | media errors **2**, available spare 99% (thr 10%), percentage used 4%, ~33.9 TB written, 379 unsafe shutdowns, critical warning 0 |
| WD Blue WD10EZEX 1TB (HDD) | `D:` | PASSED | 32,551 | 31 °C | reallocated 0, pending 0, offline uncorrectable 0, CRC errors 0 |
| WD Green WDS240G2G0A 240GB (SATA SSD) | `C:` | PASSED | 28,100 | 25 °C | reallocated 1, reported uncorrectable 0, reserved space 98%, ~99,228 GiB host writes (above the drive's ~80 TBW rating) |

Takeaways:

- `F:`: the 2 media errors match the known bad-block events. Wear is negligible; watch for the media error count to **increase**, which is the trigger for the replace-NVMe runbook.
- `D:`: clean. Fine for the cold tier.
- `C:`: healthier than expected at the SMART level, but well past rated endurance. The plan is unchanged: keep the boot loader there for now, put nothing new on it, and retire it per PHASE0_PLAN 3.1.

### 3.1 Boot moved to the NVMe

- User shrank `F:` and created a 300 MB EFI partition (disk 2, partition 2), then ran `bcdboot F:\Windows /s S: /f UEFI` and set the NVMe first in BIOS.
- Verified after reboot (2026-10-01 21:16): disk 2 (Samsung 980) reports `IsBoot` and `IsSystem`; the booted ESP is the new 300 MB partition; `F:\pagefile.sys` is the only page file.
- `C:` (disk 1) still has its old 100 MB ESP as a fallback boot entry.
- 2026-10-01 21:41: the WD SSD's SATA port was disabled in BIOS (ASUS PRIME Z490-P, BIOS 0402). Windows booted normally, and only the Samsung 980 and WD Blue are visible. **`C:` is retired from the boot path.**

## 2026-10-01: WS1 toolchain pass

| Task | Result |
|---|---|
| 1.3 Data roots | Created `F:\Nebula\{state,logs,models,worktrees,build,setup}` and `D:\NebulaCold\{models-archive,logs-archive,research,backups-tmp,backups-local,downloads,wsl}` |
| 1.4 NVIDIA driver | Already current: 596.49, CUDA 13.2 (requirement: at least 12.4) |
| 1.5 VS Build Tools | Already installed: VS Build Tools 2022 17.14.32 with the VC x64 tools |
| 1.7 Rust | rustup stable (MSVC host) was already installed; `cargo-nextest` and `cargo-sweep` installed via `cargo install --locked`; sccache 0.18.0 via winget |
| 1.8 CLI tools | Already present: git 2.52, gh 2.89, uv 0.10.4, Node 24.14.1, cmake 4.3.2, rclone 1.75.1. Newly installed: ninja 1.13.2, gitleaks 8.30.1 |
| 1.9 WSL2 | Already enabled (default version 2; the existing distro is `docker-desktop`). `%USERPROFILE%\.wslconfig` sets `memory=6GB`, which also caps Docker Desktop's VM |
| 1.11 Env vars | Set at **user** scope (single-user machine, no elevation needed): `SCCACHE_CACHE_SIZE=6G`, `SCCACHE_DIR=F:\Nebula\build\sccache`, `RUSTC_WRAPPER=sccache`, `CARGO_TARGET_DIR=F:\Nebula\build\target` |

Things on this machine Nebula has to account for:

- **Ollama, LM Studio and a `llama-cpp-turboquant` build are on PATH.** If any of them holds VRAM while llama-server starts, it will hit out-of-memory errors. `nebula doctor` / `nebula-resources` should detect other GPU processes.
- `llvm-mingw` is on PATH. The Rust default host is MSVC, so this is harmless today, but builds of C dependencies should be checked to confirm they pick MSVC.
- Docker Desktop's WSL distro shares the 6 GB `.wslconfig` cap.

### 1.12 Recovery USB

Pending: needs a physical USB stick (8 GB+) and the user. Use the Windows 11 Media Creation Tool.

## 2026-10-02: WS3 runtime and first smoke test

**Runtime (3.1).** `prism-b10743-adfffbe`, win-cuda-12.4-x64, unpacked to `F:\Nebula\runtime\llama-prism\b10743-adfffbe\` (1.1 GB). Archives are kept in `D:\NebulaCold\downloads\`. `--list-devices` shows `CUDA0: RTX 4070 (12281 MiB)`, so PrismML issue #241 (CUDA builds not starting on some CPUs) does not affect this machine.

**Models (3.3).** In `F:\Nebula\models\bonsai2-27b\`: PTQ1_0 (5.95 GB, SHA-256 verified) and mmproj Q8_0. PQ2_0 is still downloading. **No dspark drafter exists for Bonsai 2 27B** (Bonsai-demo `SPECULATIVE.md`), so B4 and the `burst` profile are parked.

**Smoke test (3.6), `standard` profile** (PTQ1_0, 32K context, f16 KV, `-np 1`, flash attention on): all checks pass.

| Measure | Value |
|---|---|
| Load time (warm NVMe) | 4.5–5.4 s |
| Offload | 65/65 layers; CUDA0 model buffer 5395 MiB, plus 265 MiB of embeddings mapped on the CPU |
| KV cache, 32K f16 | 2048 MiB (exactly 64 KiB/token, matching the design) |
| Recurrent state | 150 MiB |
| Compute buffer | 166 MiB on the GPU, 52 MiB on the host |
| VRAM | Desktop baseline 1.4 GB; loaded 9.3 GB of 12.0 GB; **~2.7 GB headroom** |
| Prompt processing (short prompts) | ~175–200 t/s (not representative; B1 measures long prompts) |
| Generation | 30–50 t/s |

Findings that changed the design (NEBULA_DESIGN 5.5.1):

- **llama-server allows every CORS origin by default and has no API key.** Any web page open in a browser could call it on `127.0.0.1`. The bench launcher now passes a random key in `LLAMA_API_KEY` and sets `--cors-origins http://nebula.invalid --no-cors-credentials`. The smoke `auth` check verifies a 401 without the key.
- The default log verbosity hides the load details; `-lv 4` shows them.
- The **desktop baseline is 1.4 GB** of VRAM (Discord, Firefox, Steam, Overwolf, the NVIDIA overlay and others). Windows (WDDM) doesn't report per-process VRAM (`N/A`), so `nebula-resources` must work from the total used.
- PrismML's KNOWN_ISSUES rules are now in the design doc: reasoning effort per role, one system message, no echoed reasoning, `"{}"` for tool calls with no arguments, and no `q5_0` KV cache.

## 2026-10-02: WS3 overnight benchmark run

Unattended run of `uv run python -m nebula_bench.run_all` from 02:06 to ~07:00. The results and report are in [bench/results/2026-10-02/](../../bench/results/2026-10-02/); the decisions are in ADR-004 and ADR-005.

- **Harness:** each stage saves `raw/<stage>.json` as soon as it finishes, and a re-run skips stages that are already done. It was restarted twice on stage boundaries (once to reorder stages so the fallback models ran before the long reasoning runs, once to add the hard needle test). The run holds `SetThreadExecutionState` so the PC can't sleep.
- **Stock runtime:** llama.cpp `b11342` (upstream) was added to `config/runtime.lock.toml` for the fallback models. PrismML's fork is 600 builds behind.
- **Fallback downloads:** Ornith 1.0 9B, Ornith 1.5 9B and DeltaCoder 9B v1.1 DPO, all Q6_K. Their hashes match the Hugging Face LFS oids and are recorded in `config/models.lock.toml`. Ornith 1.5 was released after the plan was written, so it was added as a third candidate.
- **The B6 scorer had a bug:** it normalized backslashes on the model's arguments only (meant for Windows paths), so regex patterns like `unwrap\(\)` could never match. `toolcalls.rescore` re-scores the stored failures. Search patterns now pass if the regex finds the intended text. The report shows both numbers.
- **The easy needle test saturated** (5/5 everywhere), so a 20-needle variant with near-identical names was added. That also showed no difference from the KV bias.
- The 9B fallbacks process prompts ~3x faster than Bonsai (~3,000 t/s against ~1,050 at 32K) and generate ~25% faster (51 against 41 t/s). They need far more thinking tokens to reach a lower pass rate.
