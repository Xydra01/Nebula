# Issue Plan: H20 — In-Engine Tool Grammar & Function Calling Parser

| Field | Value |
| :--- | :--- |
| Issue ID | H20 |
| Title | `[hydrogen] feat(grammar): GPU-fused FSM tool grammar masking and native IPC tool call frame generation` |
| Phase | Phase 6: Native Engine & Autonomous Agent Execution |
| Labels | `hydrogen`, `phase-6`, `core`, `tools` |
| Target Files | `hydrogen/include/tool_barrier.hpp`, `hydrogen/include/fsm_grammar.hpp`, `hydrogen/src/grammar/fsm_schema.cpp`, `hydrogen/src/model/native_decoder.cu`, `hydrogen/src/server_main.cpp` |

---

## 1. Problem Statement

To complete autonomous tasks in Nebula, the model must invoke tools (`fs.read`, `fs.write`, `shell.run`, `git.*`) via structured JSON. In the baseline architecture, CPU regex parsing and JSON deserialization stall the agent loop by ~3.2 ms per turn, and unconstrained token sampling risks malformed JSON syntax errors. Hydrogen addresses this by pre-compiling JSON schemas into GPU FSM bitmasks (Issue H11). However, H11 has not been connected to the live decoding loop in `NativeDecoder`, and the server does not serialize `MessageType::ToolCall` frames over the Win32 shared memory IPC protocol.

## 2. Implementation Plan

1. **Connect GPU-Fused FSM Bitmask Sampler**:
   - When a request specifies `tools` or `response_schema`, compile schemas into state-transition tables.
   - At each decode step within a tool call context, launch `kernel_fsm_mask_logits<<<...>>>` to bitmask illegal tokens in SRAM before sampling.
2. **Tool Call Syntax Detection**:
   - Detect tool call initiators: `<tool_call>\n{"name": "...", "arguments": ...}</tool_call>` or direct schema token generation.
   - Extract tool call ID, name, and arguments into strongly typed descriptors.
3. **IPC Tool Call Framing**:
   - When a tool call is fully generated, serialize a `MessageType::ToolCall` frame into the shared memory response ring buffer:
     ```json
     {
       "id": "call_123",
       "name": "fs.read",
       "arguments": "{\"path\": \"src/main.rs\"}"
     }
     ```
   - Set frame flag `FrameFlags::END_OF_STREAM` to signal turn completion to the daemon executor.
4. **Tool Observation Multi-Turn Injection**:
   - When the executor returns a `Role::Tool` observation frame, append the observation directly to the conversation history (`<|im_start|>tool\n{observation}<|im_end|>\n<|im_start|>assistant\n`) without resetting or recomputing the shared system prompt KV cache.

## 3. Verification & Proof Plan

- Run 100 automated tool generation trials across diverse schemas (`fs.read`, `fs.write`, `git.diff`, `cargo test`).
- Verify 100% of generated tool calls parse into valid JSON objects matching their target schema with zero syntax errors.
- Benchmark grammar masking overhead: verify FSM bitmask evaluation adds $< 10\ \mu\text{s}$ per token step.

## 4. Acceptance Criteria

- [ ] GPU-Fused FSM bitmask sampler prevents syntax errors across all tool-call generations.
- [ ] `MessageType::ToolCall` IPC frames are accurately constructed and deserialized by `crates/nebula-model/src/hydrogen.rs`.
- [ ] Multi-turn tool execution loop runs smoothly: model outputs tool $\to$ executor runs tool $\to$ model ingests observation and continues generation.
