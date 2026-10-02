# ADR-004: Model profiles for Bonsai 2 27B

**Status:** Accepted (2026-10-02)

## Context

The design doc (Section 2.6) lists model profiles with estimated numbers. Phase 0 WS3 measured them on the target machine (RTX 4070 12 GB, PrismML llama.cpp `prism-b10743-adfffbe`, CUDA 12.4) with the `bench/` harness. Full tables are in [bench/results/2026-10-02/report.md](../../bench/results/2026-10-02/report.md); raw JSON is next to it.

Key measurements (VRAM is the whole GPU, including ~1.4 GB used by the desktop):

| Config | VRAM | Headroom | Prompt t/s | Gen t/s at depth | Gen t/s short |
| --- | --- | --- | --- | --- | --- |
| PTQ1_0, f16 KV, 32K | 9.36 GB | 2.9 GB | 1055 | 40.8 | 50.1 |
| PTQ1_0, f16 KV, 64K | 11.44 GB | 0.8 GB | 913 | 34.9 | 50.1 |
| PTQ1_0, q4_0 KV, 32K | 7.97 GB | 4.3 GB | 1069 | 37.9 | 49.4 |
| PTQ1_0, q4_0 KV, 128K | 10.20 GB | 2.1 GB | 673 | 22.2 | 49.0 |
| PQ2_0, f16 KV, 32K | 10.60 GB | 1.7 GB | 1074 | 39.9 | 48.5 |

- **PQ2_0 is not better.** It runs at the same speed as PTQ1_0 (within 2%), needs 1.2 GB more VRAM, cannot fit 64K with f16 KV, and scored 12/20 against PTQ1_0's 13/20 on the coding tasks without reasoning and 18/20 against 19/20 with `medium`.
- **Reasoning effort matters more than weights.** PTQ1_0 passes 13/20 coding tasks with reasoning off and **19/20 with `medium`**, at ~7x the tokens (2,859 average vs 414) and ~6x the wall time. The longest answer used 14,547 of the 16,384-token budget.
- **Long context works.** Needle retrieval was 5/5 at 32K, 64K and 128K in every KV configuration. A harder test (20 near-identical constant names) scored 18–20/20 in all configurations.
- **The q4_0 KV calibration bias made no measurable difference.** Hard-needle totals were 38/40 with the bias and 38/40 without; the misses fell on different configurations.
- **Prompt cache:** a cached 20K-token prefix drops prompt time from 18.3 s to 0.24 s (77x). Reordering the tool list invalidates the whole cache. In an 8-turn agent loop, each turn processes only the new ~300–900 tokens.
- **Tool calls:** 100% parseable in all three modes; correct tool and arguments 98% (native tools API, prompted JSON and `json_schema` mode alike).
- **Reload:** 4.4 s for PTQ1_0 and ~5 s for PQ2_0 from a warm file cache.

## Decision

1. **PTQ1_0 is the only Bonsai weight file.** PQ2_0 is archived to `D:\NebulaCold\models-archive\` and the `quality` profile is dropped.
2. Profiles:

| Profile | KV | Context | Flags beyond the common set | Use |
| --- | --- | --- | --- | --- |
| `standard` | f16 | 32K | — | Default for every role |
| `long` | q4_0 + bias | 128K | `-ctk q4_0 -ctv q4_0 --kv-mean-center <bias>`, env `LLAMA_ATTN_ROT_DISABLE=1` | Explicit big reads and syntheses; expect ~3 min to ingest 120K tokens cold |
| `lean` | q4_0 + bias | 16K | as `long` | When VRAM is contested |
| `vision` | f16 | 24K | `--mmproj <file>` | Screenshots (not benchmarked in Phase 0) |
| `burst` | — | — | — | Stays parked until a Bonsai 2 drafter exists |

Common flags (all profiles): `-ngl 99 -fa on -np 1 --jinja --reasoning auto --ctx-checkpoints 32 --cache-ram 4096 --cache-idle-slots --cors-origins http://nebula.invalid --no-cors-credentials`, plus a per-launch `LLAMA_API_KEY`.

3. **Reasoning effort by role:** `none` for routing, tool selection and summaries; `medium` for writing or fixing code; `max_tokens` of at least 16K whenever reasoning is on (24K for known-hard steps, since one answer used 14.5K). `xhigh` is not measured and stays opt-in.
4. **Prompt layout rule (Section 5.5):** the system prompt and tool list are byte-stable across calls in a session (fixed tool order, no timestamps). Per-call content goes at the end.
5. **Keep the KV bias** in `long` and `lean`: PrismML recommends it, it costs nothing at runtime, and no harm was measured. Revisit with a Nebula-specific calibration corpus.
6. **Game-mode resume cooldown** can be short (30–60 s): a reload costs ~5 s plus ~18 s to rebuild a 20K-token prompt cache.

## Options considered

- **PQ2_0 as default:** rejected; no speed or quality gain for 1.2 GB more VRAM.
- **64K f16 as `standard`:** fits, but leaves 0.8 GB headroom, which a browser or game launcher can take. 32K keeps 2.9 GB free and matches the design's short-context strategy.
- **q4_0 for `standard`:** saves 1.4 GB at 32K for a 7% generation slowdown. Not needed while 32K f16 fits comfortably; `lean` covers the contested case.
- **Dropping the KV bias:** possible, since it showed no effect; kept for now on the vendor's advice.

## Consequences

- `config/default.toml` (Phase 1, WS5) takes these profiles verbatim; the supervisor sets `LLAMA_ATTN_ROT_DISABLE=1` only for bias profiles.
- Design doc Sections 2.3 and 2.6 get the measured numbers when this ADR is accepted.
- The coding-task suite (20 tasks) is close to saturated at `medium` (19/20). A harder suite is needed before comparing `medium` with `xhigh`.
