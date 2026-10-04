# Upgrading a Model Runtime

How to move `llama-prism` (PrismML fork, for Bonsai) or `llama-stock` (upstream, for the fallback and embedding models) to a new llama.cpp release. Runtimes are pinned in [`config/runtime.lock.toml`](../../config/runtime.lock.toml) and change only after the checks below pass.

**When:** a release fixes something we hit, adds a feature a profile needs, or the doctor's `runtime.*` check warns that the installed build doesn't match the pin. Never upgrade both runtimes in one go; do one, run the checks, merge, then the other.

## 1. Choose the release

- **PrismML:** [PrismML-Eng/llama.cpp releases](https://github.com/PrismML-Eng/llama.cpp/releases). Read `KNOWN_ISSUES` and the release notes for Bonsai changes (design Section 5.5.1 lists the request rules we depend on).
- **Stock:** [ggml-org/llama.cpp releases](https://github.com/ggml-org/llama.cpp/releases).
- Take the **`win-cuda-12.4-x64`** build. CUDA 13.x builds of the PrismML fork crash on Windows (PrismML llama.cpp #222), and 13.3 needs a newer driver than ours. Check the driver with `nvidia-smi` first if a release says it needs one.

## 2. Install side by side

The old build stays where it is until the new one is proven, so rolling back is a config change.

```powershell
$R = 'prism-bNNNNN-xxxxxxx'                       # release tag
$D = "F:\Nebula\runtime\llama-prism\bNNNNN-xxxxxxx"  # new folder, next to the old one
# Download the build zip and cudart-llama-bin-win-cuda-12.4-x64.zip to D:\NebulaCold\downloads\
Get-FileHash D:\NebulaCold\downloads\*.zip -Algorithm SHA256
Expand-Archive D:\NebulaCold\downloads\<build>.zip -DestinationPath $D
Expand-Archive D:\NebulaCold\downloads\cudart-llama-bin-win-cuda-12.4-x64.zip -DestinationPath $D
& "$D\llama-server.exe" --version
& "$D\llama-server.exe" --list-devices   # expect CUDA0: NVIDIA GeForce RTX 4070
```

## 3. Test it

Point the bench at the new build by editing `RUNTIMES` in `bench/nebula_bench/server.py`, then from `bench\`, with other GPU users closed:

```powershell
uv run nebula-smoke --profile pq2mtp     # standard: chat, tool calls, json_schema, offload, auth
uv run nebula-smoke --profile standard   # the PTQ1_0 file used by long/lean, if the prism runtime changed
uv run python -m nebula_bench.embed      # if the stock runtime changed
```

All checks must pass. Then compare speed and quality against the last report in `bench/results/`:

- **Minor release (bug fixes):** the smoke tests, plus a few `nebula chat` turns after step 4. Each turn's `model.call` log event has prompt and generation t/s; they should be within ~5% of the last report.
- **Anything touching Bonsai's quant kernels, MTP or the KV cache:** the full overnight run (`uv run python -m nebula_bench.run_all`), then compare quality scores and needle results. A drop in quality blocks the upgrade even if it's faster.

Write the results to `bench/results/<date>/` with a short `report.md`.

## 4. Pin it

1. Update the runtime's section in `config/runtime.lock.toml`: `release`, `commit`, `build`, `version`, `published`, `install_dir`, `source`, the archive hashes and `[...verified]`.
2. Update `[model.runtimes]` in `config/default.toml` and `RUNTIMES` in `bench/nebula_bench/server.py` to the new `llama-server.exe`.
3. Update the version in `docs/ops/setup.md` Section 7.
4. **Reinstall the binaries** (`nebula daemon stop`, then `scripts\install-nebula.ps1`). The lock files are compiled into `nebula-daemon`, so doctor keeps checking the old pin until you do.
5. `nebula daemon start`, `nebula chat` a couple of turns, and `nebula doctor`: `runtime.llama-prism` (or `-stock`) should say the build matches.

Open a PR with the lock, config and bench report together.

## 5. Clean up and roll back

- After a week without problems, move the old runtime folder to `D:\NebulaCold\models-archive\runtime\` (a move, not a delete).
- **Rollback:** revert the PR (or the three path changes), reinstall the binaries and restart the daemon. The old folder is still on disk or in the archive.
