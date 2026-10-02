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
