# ADR-006: MTP speculative decoding for the `standard` profile

**Status:** Accepted (2026-10-04)

## Context

ADR-004 parked the `burst` profile because PrismML has not released a drafter for Bonsai 2 27B. Two things have changed since:

- PrismML PR #205 (merged 2026-09-21, included in our `prism-b10743-adfffbe`) lets `--spec-type draft-mtp` start on Hadamard-folded Bonsai files.
- A community MTP head exists: [ProCreations/Ternary-Bonsai-2-27B-MTP](https://huggingface.co/ProCreations/Ternary-Bonsai-2-27B-MTP), Qwen3.8-27B's MTP layer trained further on Bonsai's own outputs (Apache-2.0). It ships inside the PQ2_0 file: `Ternary-Bonsai-2-27B-PQ2_0-MTP-Q8_0.gguf`, the unchanged PQ2_0 base plus one Q8_0 layer (7.66 GB).

B4 measured it on the RTX 4070 with the same harness and sampling presets as ADR-004. Full results: [bench/results/2026-10-03/report.md](../../bench/results/2026-10-03/report.md).

| Config (32K) | Gen t/s short | Gen t/s at depth | Six-workload aggregate | `medium` coding tasks | VRAM / headroom |
| --- | --- | --- | --- | --- | --- |
| PTQ1_0, f16, no MTP (ADR-004 `standard`) | 50.1 | 40.8 | 48.7 | 19/20 (2026-10-02, one run) | 9.36 / 2.9 GB |
| PQ2_0, q4_0, MTP off | 47.3 | 37.0 | 45.8 | 17/20, 17/20 | 9.57 / 2.7 GB |
| **PQ2_0, q4_0, MTP 2 draft tokens** | **84.1** | **60.5** | **72.2 (1.58x)** | **16/20, 15/20** | **10.24 / 2.0 GB** |
| PTQ1_0 + grafted head, f16, MTP 1 draft token | — | — | 53.1 (1.09x) | — | 10.60 / 1.7 GB |

- **PQ2_0 + MTP is 1.58x faster overall**: 1.7x on short prompts, 1.6x at 32K depth, 1.58x on the real coding tasks (median 79 vs 50 t/s). Rewriting existing code gains most (up to 2.2x); prose least (1.26x). Acceptance is 78% at 2 draft tokens.
- **The head works on PTQ1_0 but doesn't pay.** Grafted onto the PTQ1_0 file (byte-identical Hadamard metadata, same tensor layout), it accepts drafts at the same rate as on PQ2_0, but verifying a 2–6-token batch is slow on PTQ1_0's CUDA kernel (Bonsai KNOWN_ISSUES; PrismML PR #218, in review). Best case 1.09x; from 2 draft tokens up it is slower than no drafting.
- **Quality.** llama.cpp's acceptance rule is exact (the target samples every token; drafts only skip work), so MTP cannot change the output distribution except through batched-kernel numerics, which flip near-tied tokens (seen in a greedy check). Measured: `medium` 15.5/20 with MTP vs 17/20 for the same file with MTP off; reasoning off went the other way (14.5 vs 12). Within noise for 20 tasks, but a ~1-task cost cannot be ruled out. Tool calls: native 100% tool / 99% args; `json_schema` 96% / 96%. Hard needle 20/20 at 32K, 19/20 at 64K. Prompt cache 65x on a 20K prefix with the same agent-loop pattern as before.
- **Against ADR-004's `standard` the switch also means PQ2_0 instead of PTQ1_0.** On 2026-10-02 PQ2_0 scored one task below PTQ1_0 in both reasoning modes (single runs). Counting that, `standard` may lose 1–3 of 20 coding tasks in exchange for ~1.6x generation speed.

## Decision

1. **`standard` uses `Ternary-Bonsai-2-27B-PQ2_0-MTP-Q8_0.gguf` with `--spec-type draft-mtp --spec-draft-n-max 2` and q4_0 KV at 32K** (`bench/profiles/pq2mtp.toml`). f16 KV does not fit next to the head with a safe margin (0.66 GB headroom). No KV bias, as in ADR-004's `standard`.
2. **`burst` is dropped as a separate profile**; speculation is part of `standard`.
3. **`long`, `lean` and `vision` stay as in ADR-004, on PTQ1_0.** PQ2_0 + MTP at 128K q4_0 runs at 34 t/s (vs 22) but leaves 0.6 GB headroom and crashed while saving a 2.7 GB slot state to the host-RAM prompt cache. `lean` exists for contested VRAM, where PTQ1_0 without a head is smallest. `--mmproj` with MTP is untested.
4. **Both weight files stay on `F:`**: PTQ1_0 (5.95 GB) and PQ2_0+MTP (7.66 GB). The grafted PTQ1_0+MTP file is archived on `D:` for the re-test below.
5. **Re-test PTQ1_0 + MTP when PrismML #218 (or another PTQ1_0 small-batch decode fix) ships** (`uv run python -m nebula_bench.mtp`). If it reaches ~1.4x, return to a single PTQ1_0 file with the grafted head.

## Options considered

- **PTQ1_0 + grafted head:** works, 1.09x at best. Revisit after #218 (decision 5).
- **PQ2_0 + MTP with q8_0 KV:** same speed as q4_0 (61 t/s at depth) with 1.53 GB headroom instead of 2.04. q4_0 is the configuration every quality test ran on, and it keeps more margin.
- **PQ2_0 + MTP with f16 KV:** fastest at depth (71.6 t/s) but 0.66 GB headroom.
- **3–5 draft tokens:** better for rewriting files (up to 101 t/s), worse for prose and reasoning; 2 is the best single setting. Per-request draft length is a possible Phase 1 refinement.
- **PQ2_0 + MTP as `long`:** 34 t/s at 128K is attractive, but see decision 3.
- **Unsloth's separate `mtp-Qwen3.8-27B-Q4_0.gguf`:** not tested; PrismML PR #205 measured the separate-file setup on Bonsai as a net loss (duplicate 248K-token vocabulary).
- **N-gram drafting (no model):** silently does nothing on Bonsai (PrismML issue #203, open).
- **ProCreations' DFlash2 drafter:** needs their patched runtime rather than an official PrismML release.

## Consequences

- `standard` generation goes from ~50/41 t/s (short/depth) to ~84/60 t/s; prompt processing drops ~3%. Headroom at 32K goes from 2.9 GB to 2.0 GB.
- Switching between `standard` and `long`/`lean`/`vision` now changes the weight file. Profile switches already restart llama-server (~5 s), so the cost is the same.
- `standard` depends on a community artifact pinned by SHA-256 in `config/models.lock.toml`. If a future PrismML release stops loading its MTP layer, the supervisor restarts `standard` with `--spec-type none` (PQ2_0 at ~46 t/s) and `doctor` reports it.
- Keep `--cache-ram 4096` for `standard` (the 20K-token save works). Any profile that combines MTP with more than 64K context must use `--cache-ram 0` until the crash is understood.
- Design doc Sections 2.2, 2.3, 2.6 and 5.5.1 and the disk budget were updated on acceptance.
