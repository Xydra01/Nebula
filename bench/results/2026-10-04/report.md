# 3.5: CPU embedding server (2026-10-04)

**Result: Qwen3-Embedding-0.6B Q8_0 works as the CPU embedding server.** It loads on the CPU in ~1.3 s, uses no VRAM, ranks the right function first for 8 of 10 code-search queries (all 10 in the top 5), and slows the `standard` chat model's generation by 1–3% while it is indexing.

- Model: `Qwen/Qwen3-Embedding-0.6B-GGUF`, `Qwen3-Embedding-0.6B-Q8_0.gguf` (639 MB, SHA-256 matches the LFS oid), Apache-2.0. 1024 dimensions (Matryoshka, can be cut to 32–1024), last-token pooling, trained for 32K tokens.
- Runtime: stock llama.cpp `b11342`, `--embedding --pooling last --device none -ngl 0 -t 8`. Profile: [`bench/profiles/embedding.toml`](../../profiles/embedding.toml). Script: `uv run python -m nebula_bench.embed`. Raw data: [`raw/embedding.json`](raw/embedding.json).
- Queries get the prefix `Instruct: <task>\nQuery:`; documents get none (model card).

## Memory and batch size

On the CPU, the compute buffer grows with the batch size (~0.6 MiB per token), while speed stays flat. The profile caps inputs at 1024 tokens.

| `-ub` / ctx / slots | Compute buffer | KV | Private RAM idle → after indexing | Speed |
| --- | --- | --- | --- | --- |
| 4096 / 8192 / 2 | 2,498 MiB | 896 MiB | 3,881 → 5,026 MiB | 222 tok/s |
| 4096 / 8192 / 2, `-fa on` | 2,418 MiB | 896 MiB | 3,800 → 4,919 MiB | 239 tok/s |
| 2048 / 4096 / 1, `-fa on` | 1,209 MiB | 448 MiB | 2,140 → 2,559 MiB | 244 tok/s |
| **1024 / 1024 / 1, `-fa on` (chosen)** | 618 MiB | 112 MiB | **1,212 → 1,781 MiB** | 246 tok/s |

The weights (604 MiB) are memory-mapped from the file and not counted in private RAM. Inputs over 1024 tokens get HTTP 400 (`exceed_context_size_error`) and the server keeps running, so callers must chunk.

## Throughput

| Measure | Result |
| --- | --- |
| Index 90 function chunks (16,670 tokens, 16 per request) | 67.8 s, **246 tokens/s** |
| One 968-token input | 4.3 s |
| Query embedding (10 in one request) | 156 ms each |

At ~250 tokens/s, a first full index of a 100K-line repo (~1.5M tokens) takes ~1.7 hours. That should run in the background, with incremental updates after file changes.

## Retrieval sanity check

Ten plain-English descriptions of functions in `bench/nebula_bench/`, searched against all 90 functions and methods in that package.

- **Top 1: 8/10. Top 5: 10/10.**
- Both misses ranked 2nd, behind reasonable neighbours: `count_tokens` lost to its near-duplicate `token_count`, and `Server._wait_healthy` lost to `Server.__enter__`, which calls it.

This is a sanity check, not a benchmark. The Phase 2 code index needs its own evaluation on a real repo.

## Contention with `standard` (PQ2_0 + MTP on the GPU)

Raw `/completion`, 512 tokens, 3 runs each. The embedding server indexed in a loop during the second set.

| Embedding threads | `standard` alone | While embedding | Change | Chunks embedded meanwhile |
| --- | --- | --- | --- | --- |
| 8 | 70.4–72.8 t/s | 69.2–69.4 t/s | −2 to −3% | 48 |
| 4 | 69.8–70.0 t/s | 68.6–69.2 t/s | −1% | 32 |

An earlier run with the 4096-token batch cost 6% (69 → 65 t/s), so the smaller batch also reduces interference. The profile uses 8 threads; the daemon can drop to 4 while a chat request is generating.

## Notes

- `llama-server` with `--device none` does not show up in the GPU process list, and VRAM did not change when it stopped. The 136 MiB difference in `raw/embedding.json` is the desktop's VRAM use fluctuating.
- Alternatives considered: EmbeddingGemma-300M (2K-token context, Gemma licence) and CodeRankEmbed 137M (code-only, 2K context). Qwen3-Embedding-0.6B matches the design's "~0.6B class" and handles both code and prose, which Smart Archive v2 will also need.
