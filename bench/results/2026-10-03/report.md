# B4: MTP speculative decoding on Bonsai 2 27B (2026-10-03)

RTX 4070 12 GB, PrismML llama.cpp `prism-b10743-adfffbe` (includes PrismML PR #205, which lets `--spec-type draft-mtp` start on Hadamard-folded Bonsai files). Decision: [ADR-006](../../../docs/adr/ADR-006-mtp-speculative-decoding.md). VRAM is the whole GPU including the desktop (~1.4 GB), in MiB/1000 as in ADR-004. Raw JSON is in `raw/`; the harness is `nebula_bench/mtp.py` (sweep) and `nebula_bench/mtp_validate.py` (validation).

## Drafter

There is no PrismML drafter for Bonsai 2. The drafter tested is the community MTP head [ProCreations/Ternary-Bonsai-2-27B-MTP](https://huggingface.co/ProCreations/Ternary-Bonsai-2-27B-MTP): Qwen3.8-27B's MTP layer, trained further on Bonsai's own outputs, shipped inside `Ternary-Bonsai-2-27B-PQ2_0-MTP-Q8_0.gguf` (the unchanged PQ2_0 base plus one Q8_0 `blk.64` layer; 7.66 GB, SHA-256 verified).

Unsloth's separate 1.37 GB `mtp-Qwen3.8-27B-Q4_0.gguf` was not tested: PrismML PR #205 measured the separate-file setup on Bonsai as a net loss, because the sidecar carries its own copy of the 248K-token vocabulary.

**PTQ1_0 graft.** Both Bonsai files have byte-identical `prism.hadamard.*` metadata and the same tensor layout, so `nebula_bench/graft_mtp.py` copies the 15 head tensors onto the PTQ1_0 file (`block_count` 65, `nextn_predict_layers` 1). The grafted file loads and drafts normally.

## Sweep

Six chat workloads, two repeats each, with the profile's sampling presets (instruct for reasoning off, thinking for `medium`), 32K context, `-np 1`. "Aggregate" is total generated tokens over total generation time. The "off" rows are the same file with `--spec-type none`.

| Model | KV | Draft tokens | Aggregate t/s | Acceptance | Peak VRAM | code_write | code_edit | json | prose | reasoning_medium | long_ctx (20K prompt) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| PQ2_0+MTP | q4_0 | off | 45.8 | — | 9.57 GB | 46.2 | 45.4 | 46.3 | 45.8 | 47.8 | 39.1 |
| PQ2_0+MTP | q4_0 | 1 | 60.1 | 84% | 10.40 GB | 61.7 | 64.1 | 62.9 | 53.7 | 61.3 | 49.1 |
| **PQ2_0+MTP** | **q4_0** | **2** | **72.2** | **78%** | **10.55 GB** | **70.0** | **81.5** | **77.6** | **57.7** | **74.3** | **55.1** |
| PQ2_0+MTP | q4_0 | 3 | 69.3 | 67% | 10.70 GB | 71.4 | 88.2 | 78.7 | 50.3 | 70.7 | 47.2 |
| PQ2_0+MTP | q4_0 | 4 | 70.5 | 58% | 10.84 GB | 68.3 | 99.2 | 83.1 | 48.4 | 67.9 | 48.7 |
| PQ2_0+MTP | q4_0 | 5 | 67.0 | 52% | 10.99 GB | 67.3 | 101.3 | 79.6 | 44.3 | 59.5 | 56.6 |
| PTQ1_0+MTP | f16 | off | 48.7 | — | 9.66 GB | 47.5 | 48.3 | 49.7 | 48.5 | 50.7 | 43.4 |
| PTQ1_0+MTP | f16 | 1 | 53.1 | 85% | 10.60 GB | 53.6 | 56.8 | 55.0 | 47.7 | 52.1 | 47.8 |
| PTQ1_0+MTP | f16 | 2 | 48.9 | 76% | 10.71 GB | 47.8 | 56.4 | 52.5 | 40.1 | 48.2 | 41.5 |
| PTQ1_0+MTP | f16 | 3 | 46.9 | 68% | 10.88 GB | 44.7 | 59.6 | 52.5 | 34.3 | 44.1 | 41.6 |
| PTQ1_0+MTP | f16 | 4 | 48.0 | 57% | 11.00 GB | 45.8 | 69.2 | 56.4 | 31.2 | 45.8 | 39.3 |
| PTQ1_0+MTP | f16 | 5 | 45.2 | 52% | 11.15 GB | 41.4 | 69.0 | 54.4 | 29.1 | 42.4 | 37.3 |
| PTQ1_0+MTP | q4_0 | off | 47.2 | — | 8.39 GB | 47.6 | 46.8 | 47.6 | 47.6 | 49.4 | 39.7 |
| PTQ1_0+MTP | q4_0 | 1 | 50.9 | 85% | 9.25 GB | 51.0 | 54.2 | 53.3 | 46.0 | 51.2 | 44.4 |
| PTQ1_0+MTP | q4_0 | 2 | 46.9 | 76% | 9.40 GB | 46.9 | 54.5 | 51.3 | 37.6 | 46.5 | 37.8 |
| PTQ1_0+MTP | q4_0 | 3 | 44.0 | 65% | 9.55 GB | 43.2 | 57.9 | 48.5 | 33.0 | 41.9 | 35.5 |
| PTQ1_0+MTP | q4_0 | 4 | 46.7 | 58% | 9.70 GB | 47.2 | 66.4 | 57.0 | 32.7 | 42.7 | 36.4 |
| PTQ1_0+MTP | q4_0 | 5 | 42.5 | 53% | 9.71 GB | 42.6 | 66.7 | 49.8 | 27.3 | 37.6 | 34.9 |

- **PQ2_0 + MTP, 2 draft tokens: 1.58x** overall. Repetitive output (rewriting a file) keeps improving up to 5 draft tokens (2.2x); prose peaks at 2 (1.26x). 2 is the best single setting.
- **PTQ1_0 + MTP: at most 1.09x** (1 draft token), and slower than no drafting from 2 tokens up. Acceptance is the same as on PQ2_0 (85/76/65%), so the grafted head works; the cost is in verification. Checking a draft runs the target on a small batch (2–6 tokens), and Bonsai's KNOWN_ISSUES lists PTQ1_0's small-batch CUDA decode kernel as slow (PrismML PR #218, in review). Worth re-running when that lands.
- MTP costs ~3–5% prompt-processing speed (20K prompt: 1,104–1,129 → ~1,073 t/s on PQ2_0).

## Validation of PQ2_0 + MTP (2 draft tokens)

### Throughput and VRAM (B1 method: repo-dump prompt at 90% of context, greedy, 128 tokens)

| KV | Context | Prompt t/s | Gen t/s at depth | Gen t/s short | Peak VRAM | Headroom |
| --- | --- | --- | --- | --- | --- | --- |
| f16 | 32K | 1,061 | 71.6 | 81.3 | 11.63 GB | 0.66 GB |
| q8_0 | 32K | 1,053 | 61.0 | 82.4 | 10.75 GB | 1.53 GB |
| **q4_0** | **32K** | **1,055** | **60.5** | **84.1** | **10.24 GB** | **2.04 GB** |
| q4_0 | 64K | 892 | 44.3 | 84.2 | 11.10 GB | 1.18 GB |
| q4_0 | 128K (`--cache-ram 0`) | 662 | 34.0 | 84.0 | 11.69 GB | 0.60 GB |

For comparison (2026-10-02, no MTP): PTQ1_0 f16 32K 40.8 t/s at depth / 50.1 short, 2.9 GB headroom; PTQ1_0 q4_0 128K 22.2 / 49.0.

The first 128K run crashed after its timed requests, while saving the 2.7 GB slot state (0.46 GB of it the draft context) to the host-RAM prompt cache (`--cache-ram 4096`). With `--cache-ram 0` it completed. At 20K tokens the same save (592 MB) works; see B3.

### Quality

| Test | PQ2_0 + MTP | Same file, MTP off (same day) | Earlier, no MTP (2026-10-02) |
| --- | --- | --- | --- |
| Coding tasks, reasoning `none` | 15/20, 14/20 | — | PQ2_0 12/20, PTQ1_0 13/20 |
| Coding tasks, `medium` (16K max tokens) | 16/20, 15/20 | **17/20, 17/20** | PQ2_0 18/20, PTQ1_0 19/20 |
| Tool calls, native | valid 100%, tool 100%, args 99% | — | PQ2_0 100/100/99, PTQ1_0 100/99/98 |
| Tool calls, `json_schema` | valid 100%, tool 96%, args 96% | — | PQ2_0 100/97/96, PTQ1_0 100/98/98 |
| Hard needle (20 near-identical names), q4_0 | 20/20 at 32K, 19/20 at 64K | — | PTQ1_0 q4_0 64K 20/20 |

- `medium` with MTP averaged 15.5/20 against 17/20 for the same file with MTP off. Reasoning `none` moved the other way. A single 20-task run varies by 2–3 tasks on sampling noise alone, so this is consistent with no effect, but a ~1-task cost cannot be ruled out at this sample size.
- llama.cpp's acceptance rule is exact: at each drafted position the target samples its own token with the full sampler and the draft is kept only on a match (`common_sampler_sample_and_accept_n`). Outputs can only differ through numerics: verifying a batch of tokens uses different kernels than one token at a time.
- **Greedy identity check** (4 coding prompts, `medium`, temperature 0, 2,000 tokens): two runs without MTP were identical; with MTP the text diverged after 112–627 characters, each time at a plausible word choice ("Let me think through the requirements" vs "... the algorithm"). That is the batched-numerics effect, not a broken rollback.
- `medium` generation speed on the coding tasks: median 78–79 t/s with MTP vs 50 t/s without; generation time for the 20 tasks 11.5–13.4 min vs 14.6–22.8 min.

### Prompt cache (B3, 20K-token prefix)

| | No cache | Cache | Tools reordered |
| --- | --- | --- | --- |
| PQ2_0 + MTP | 18.6 s | 0.29 s (65x) | 18.6 s |
| PTQ1_0, no MTP (2026-10-02) | 18.3 s | 0.24 s (77x) | 18.3 s |

Agent loop, tokens processed per turn: 20507, 486, 871, 686, 804, 748, 335, 891 (identical pattern to PTQ1_0).
