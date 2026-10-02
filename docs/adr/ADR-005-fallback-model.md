# ADR-005: Fallback model

**Status:** Accepted (2026-10-02)

## Context

Nebula needs a model that runs on **stock** llama.cpp in case the PrismML fork breaks or falls too far behind (ADR-003). All candidates were tested on llama.cpp `b11342` with the same harness and tasks as Bonsai (ADR-004), using each model card's sampling settings. Full results: [bench/results/2026-10-02/report.md](../../bench/results/2026-10-02/report.md).

**Round 1, dense 9B models in VRAM** (Ornith-1.0, Ornith-1.5, DeltaCoder; all Qwen 3.5 9B fine-tunes, Q6_K): the best reached 12–13/20 on the coding tasks, well short of Bonsai's 19/20.

**Round 2, mixture-of-experts (MoE) models with the routed experts in system RAM.** Only 3–4B parameters are active per token, so llama.cpp keeps attention, KV cache and some experts on the GPU and the rest in RAM (`--fit on --fit-target 1536` picks the split at launch). All three are Unsloth UD-Q4_K_XL.

| Model | Code, thinking off | Code, thinking on | Tool calls native / schema | Prompt t/s | Gen t/s 32K / 128K | VRAM / RAM | Cache: 20K prefix uncached → cached |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **Gemma 4 26B-A4B** (Apache-2.0, 17.0 GB) | **18/20** (7 min) | **19/20** (47 min) | 88% / 98% | 620 | 27.5 / 19 | 11.1 GB / 10.5 GB | 31.5 s → 0.41 s |
| Qwen3.6-35B-A3B (Apache-2.0, 22.4 GB) | 17/20 (5 min) | 15/20 (77 min) | 100% / 97% | 334 | 33 / 25 | 11.1 GB / 11.6 GB | 58.6 s → 0.31 s |
| GLM-4.7-Flash 30B-A3B (MIT, 17.5 GB) | 8/20 (9 min) | 12/20 (85 min) | 98% / 95% | 367 | 27 / 12 | 11.1 GB / 12.1 GB | not run |
| *Best 9B (DeltaCoder)* | *12/20 (3 min)* | *9/20* | *98% / 94%* | *3,000* | *51 / —* | *9.0 GB / —* | *not run* |
| *Bonsai 2 27B PTQ1_0 (primary)* | *13/20 (3 min)* | *19/20 (20 min, `medium`)* | *98% / 98%* | *1,055* | *41 / 22* | *9.4 GB / —* | *18.3 s → 0.24 s* |

All three MoE models scored 95–100% on the hard needle test at 32K, and loaded in 10–18 s. VRAM is high by design: `--fit` fills the GPU up to the 1.5 GB margin.

## Decision

**Gemma 4 26B-A4B (UD-Q4_K_XL)** is the fallback model, in `F:\Nebula\models\fallback-moe\`. Qwen3.6-35B-A3B, GLM-4.7-Flash and the three 9B models are archived to `D:\NebulaCold\models-archive\`.

The `fallback` profile (`bench/profiles/gemma4moe.toml`):

- Runtime: stock llama.cpp (`[llama-stock]` in `config/runtime.lock.toml`).
- `--fit on --fit-target 1536` instead of `-ngl 99`, with f16 KV at 32K. `--fit` re-plans the GPU/RAM split at every launch, so it adapts to whatever the desktop is using.
- `--ctx-checkpoints 1 --cache-ram 0`. Gemma 4's sliding-window layers make checkpoints very large; the in-slot prompt cache still works (see the table).
- Thinking off by default; thinking on (`enable_thinking: true`, max_tokens of at least 16K) for the roles where ADR-004 uses `medium`.
- Sampling from the model card: temperature 1.0, top_p 0.95, top_k 64.
- **Tool calls must use schema-constrained decoding** (design Section 5.6, already mandatory). Gemma's native tool-call mode was the weakest (88%); its misses were mostly reasonable but unrequested extra steps (`mkdir` before a write) or omitted optional arguments.

## Options considered

- **Qwen3.6-35B-A3B:** the runner-up, and nearly a tie: 17/20 vs 18/20 is one task. It has the best native tool calling (100%) and 20% faster generation. But thinking mode makes it *worse* (15/20 after 77 min), its prompt processing is half Gemma's speed (a cache miss on a 20K prefix costs 59 s vs 31 s), and it needs 1.1 GB more RAM. Choose it instead if native (unconstrained) tool calls turn out to matter.
- **GLM-4.7-Flash:** good tool calls, weak code (8/20, 12/20 with thinking).
- **9B dense models (round 1):** fast, but far weaker (best 12/20).
- **Caveats:** 20 tasks is a small sample; one task is 5 points. The RAM use matters: with Gemma loaded, ~10.5 GB of the 32 GB stays in use, which game mode (Section 2.6) must account for. That doesn't matter much for a fallback that runs only when the primary is broken.

## Consequences

- With Gemma, the fallback is close to Bonsai on the code tasks (18–19/20) rather than a large step down, at the cost of slower generation (27.5 vs 41 t/s) and ~10.5 GB of system RAM.
- Because `--fit` sets placement at launch, the supervisor must start the fallback only after the primary has fully released VRAM, and must not rely on a fixed `-ngl`.
- Gemma's thinking-off score (18/20 in 7 min) beats Bonsai's no-reasoning score (13/20). That is worth a closer look in Phase 1 (for example, as an executor model when Bonsai is busy), but it does not change ADR-004: Bonsai with `medium` reasoning is still as good, faster and fully in VRAM.
- The design doc Section 2.6 `fallback` row changes to Gemma 4 26B-A4B when this ADR is accepted.
