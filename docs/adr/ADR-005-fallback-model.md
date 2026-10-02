# ADR-005: Fallback model

**Status:** Proposed

## Context

Nebula needs a model that runs on **stock** llama.cpp in case the PrismML fork breaks or falls too far behind (ADR-003). The design doc named Ornith-1.0-9B with Qwen3.5-DeltaCoder-9B as the runner-up. Ornith-1.5-9B came out before Phase 0 started and was added as a third candidate. All three are Qwen 3.5 9B fine-tunes, tested as Q6_K (7.4–7.6 GB) on llama.cpp `b11342` with f16 KV at 32K, using the sampling settings from each model card. Full results: [bench/results/2026-10-02/report.md](../../bench/results/2026-10-02/report.md).

| Model | Code tasks, thinking off | Code tasks, thinking on | Tool calls (native / schema) | Gen t/s at 32K |
| --- | --- | --- | --- | --- |
| Ornith-1.0-9B (MIT) | 8/20 (2.7 min) | 13/20 (28 min) | 95% / 87% | 51 |
| Ornith-1.5-9B (MIT) | 10/20 (3.6 min) | 12/20 (46 min) | 97% / 92% | 51 |
| **DeltaCoder-9B v1.1 DPO** (Apache-2.0) | **12/20 (3.4 min)** | 9/20 (41 min; 6 answers hit the 16K cap) | **98% / 94%** | 51 |
| *Bonsai 2 27B PTQ1_0, for reference* | *13/20 (3.2 min)* | *19/20 (20 min, `medium`)* | *98% / 98%* | *41* |

All three use the same VRAM (9.0 GB total at 32K, 3.3 GB headroom) and process prompts ~3x faster than Bonsai.

## Decision

**DeltaCoder-9B v1.1 DPO Q6_K, with thinking disabled**, is the fallback model. It goes in `F:\Nebula\models\fallback\`; both Ornith models are archived to `D:\NebulaCold\models-archive\`.

The `fallback` profile: llama.cpp stock (`config/runtime.lock.toml` `[llama-stock]`), f16 KV, 32K context, `enable_thinking: false`, instruct sampling (temperature 0.7, top_p 0.8, top_k 20, presence penalty 1.5; never below temperature 0.5, per the model card). The common security and cache flags from ADR-004 apply.

## Options considered

- **Ornith-1.0-9B (design doc's original pick):** the highest single score (13/20), but only with ~5K thinking tokens per task, 8x slower than DeltaCoder. Thinking off, it was the weakest model (8/20).
- **Ornith-1.5-9B:** better than 1.0 with thinking off, but thinking used 7.7K tokens per task on average for 12/20.
- **DeltaCoder (chosen):** the best result for its cost, and the best tool calling, which matters most for a fallback that keeps the agent loop running. With thinking on it gets *worse* and loops until the token cap; the model card warns about looping.
- **Caveats:** 20 tasks is a small sample, and one task is 5 points. The Ornith model cards mention a modified chat template that was not used here (the template embedded in the GGUF was), which may understate Ornith. Neither caveat changes the main finding: none of the 9B models comes close to Bonsai with `medium` reasoning, so the fallback is for keeping the system alive, not for full-quality work.

## Consequences

- When the fallback is active, Nebula should lower its ambitions: shorter steps, more verification, and no reasoning effort settings (the role-to-effort table from ADR-004 maps to thinking off).
- The design doc Section 2.6 `fallback` row changes from Ornith-1.0 to DeltaCoder when this ADR is accepted.
- Revisit when a new 9–14B coding model appears, using the same harness (`uv run python -m nebula_bench.run_all` with a new profile).
