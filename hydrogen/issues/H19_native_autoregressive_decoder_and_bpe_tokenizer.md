# Issue Plan: H19 — Full Autoregressive Decoder Integration & Robust Tokenization

| Field | Value |
| :--- | :--- |
| Issue ID | H19 |
| Title | `[hydrogen] feat(decoder): Full 64-layer autoregressive decoder, dual BPE/SentencePiece tokenizer, and stochastic sampler` |
| Phase | Phase 6: Native Engine & Autonomous Agent Execution |
| Labels | `hydrogen`, `phase-6`, `core` |
| Target Files | `hydrogen/CMakeLists.txt`, `hydrogen/include/native_decoder.hpp`, `hydrogen/src/model/native_decoder.cu`, `hydrogen/src/server_main.cpp` |

---

## 1. Problem Statement

The Project Hydrogen server (`hydrogen/src/server_main.cpp`) currently responds to `MSG_GENERATE_REQUEST` by executing a single-layer forward baseline probe (`ForwardBaseline::run`) and returning a static synthetic string chopped into simulated word chunks. While a prototype 64-layer decoder (`hydrogen/src/model/native_decoder.cu`) exists, it is untracked, excluded from `CMakeLists.txt`, lacks a real BPE/byte-fallback tokenizer, uses naive sequential token-by-token prefill, hardcodes greedy argmax sampling, and ignores ChatML multi-turn message structures.

## 2. Implementation Plan

1. **Build System & Engine Linking**:
   - Add `src/model/native_decoder.cu` to target `hydrogen_engine` in `hydrogen/CMakeLists.txt`.
   - Ensure clean compilation under MSVC `/std:c++20` and `nvcc -O3 --use_fast_math -arch=compute_89,code=sm_89`.
2. **Dual-Mode Robust Tokenizer**:
   - Parse `tokenizer.ggml.tokens`, `tokenizer.ggml.scores`, and `tokenizer.ggml.merges` from model metadata.
   - If `tokenizer.ggml.merges` exists: execute BPE rank-based greedy pair merging.
   - If `tokenizer.ggml.merges` is missing: execute dynamic programming Viterbi token segmentation using token scores, falling back to byte tokens `<0x00>`...`<0xFF>` for raw UTF-8 octets.
   - Handle special ChatML tokens (`<|im_start|>`, `<|im_end|>`, `<tool_call>`, `</tool_call>`) as atomic, non-splittable tokens.
3. **ChatML Multi-Turn Formatter**:
   - Parse JSON request messages array (`system`, `user`, `assistant`, `tool`).
   - Format accurately according to Qwen/ChatML template:
     ```text
     <|im_start|>system\n{system_prompt}<|im_end|>\n
     <|im_start|>user\n{user_message}<|im_end|>\n
     <|im_start|>assistant\n
     ```
4. **GPU Stochastic Sampler Kernel**:
   - Implement `kernel_sample_logits`:
     - Temperature scaling: $z_i = z_i / T$.
     - Repetition penalty: apply penalty to previously generated tokens.
     - Top-P (nucleus) and Min-P filtering on device.
     - Fast on-device multinomial sampling using Philox PRNG or warp-level prefix scan.
5. **Autoregressive Loop Integration in `server_main.cpp`**:
   - Instantiate `NativeDecoder` in `server_main.cpp`.
   - Feed incoming prompt tokens into prefill, then decode autoregressively until EOS token (`eos_token_id_ = 248046` or `<|im_end|>`) or `max_tokens` cap.
   - Stream decoded token chunks live to the shared memory ring buffer.

## 3. Verification & Proof Plan

- Run `hydrogen_smoke_test -m "F:\Nebula\models\bonsai2-27b\Ternary-Bonsai-2-27B-PTQ1_0.gguf" -p "Write a Rust hello world" --json`.
- Verify generated text is coherent, syntactically valid Rust code.
- Verify perplexity delta vs. `llama-server` is strictly $\Delta \text{PPL} < 0.05$.
- Verify inter-token generation latency is strictly $\le 15.0\text{ ms}$ on RTX 4070 ($> 65\text{ tokens/s}$).

## 4. Acceptance Criteria

- [ ] `native_decoder.cu` compiles and links cleanly in `CMakeLists.txt` with zero warnings under `/W4`.
- [ ] Tokenizer correctly decodes arbitrary UTF-8 characters and code syntax via BPE/byte fallback without crashing on out-of-vocabulary inputs.
- [ ] ChatML prompt formatting produces standard multi-turn conversation traces.
- [ ] GPU stochastic sampler accurately applies temperature, top-p, and repetition penalty.
- [ ] Model autoregressively generates text, ending cleanly on EOS tokens.
