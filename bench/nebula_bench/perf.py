"""Performance benchmarks: B1 throughput, B2 VRAM, B3 prompt cache, B8 reload time."""

from __future__ import annotations

import statistics
import time

from nebula_bench.client import (
    GPU_TOTAL_MIB,
    chat,
    complete_raw,
    count_tokens,
    process_ram_mib,
    shared_gpu_mib,
    vram_used_mib,
)
from nebula_bench.corpus import repo_dump
from nebula_bench.server import Profile, Server

KV_BYTES_PER_TOKEN = {"f16": 64 * 1024, "q8_0": 34 * 1024, "q4_0": 18 * 1024}
DESKTOP_RESERVE_MIB = 1500
OVERHEAD_MIB = 600  # recurrent state + compute buffers + CUDA context, measured ~500 MiB


def fits(model_buffer_mib: int, ctx: int, kv_type: str) -> tuple[bool, int]:
    need = model_buffer_mib + ctx * KV_BYTES_PER_TOKEN[kv_type] // (1024 * 1024) + OVERHEAD_MIB
    return need + DESKTOP_RESERVE_MIB <= GPU_TOTAL_MIB, need


def build_prompt(server: Server, target_tokens: int, cache: dict) -> tuple[str, int]:
    """Repo-dump text trimmed to roughly target_tokens (within ~2%)."""
    if "sections" not in cache:
        cache["sections"] = repo_dump(target_chars=900_000)
    sections = cache["sections"]
    text, chars_per_token = "", 3.2
    lo, hi = 1, len(sections)
    # Binary search on the number of whole sections, then trim characters.
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if count_tokens(server, "".join(sections[:mid])) <= target_tokens:
            lo = mid
        else:
            hi = mid - 1
    text = "".join(sections[:lo])
    n = count_tokens(server, text)
    if n < target_tokens:
        extra = sections[lo] if lo < len(sections) else ""
        text += extra[: int((target_tokens - n) * chars_per_token)]
    while (n := count_tokens(server, text)) > target_tokens:
        text = text[: int(len(text) - (n - target_tokens) * chars_per_token) - 16]
    return text, n


def throughput(profile: Profile, ctx: int, kv_type: str, model_buffer_mib: int, log) -> dict:
    """B1 + B2 for one (model, kv_type, ctx) configuration."""
    ok, need = fits(model_buffer_mib, ctx, kv_type)
    result: dict = {"ctx": ctx, "kv_type": kv_type, "estimated_mib": need}
    if not ok:
        result["skipped"] = f"estimated {need} MiB + desktop reserve exceeds {GPU_TOTAL_MIB} MiB"
        return result

    baseline_vram, baseline_shared = vram_used_mib(), shared_gpu_mib()
    p = profile.with_(ctx=ctx, kv_type=kv_type)
    with Server(p, tag=f"b1-{kv_type}-{ctx // 1024}k") as server:
        result["load_seconds"] = round(server.load_seconds, 2)
        result["vram_loaded_mib"] = vram_used_mib()
        cache: dict = {}
        target = int(ctx * 0.9) - 256
        prompt, n_prompt = build_prompt(server, target, cache)
        result["prompt_tokens"] = n_prompt

        runs = 3 if ctx <= 32768 else 2
        pp, tg = [], []
        for i in range(runs):
            r = complete_raw(server, prompt, n_predict=128, cache_prompt=False)
            t = r["timings"]
            pp.append(t["prompt_per_second"])
            tg.append(t["predicted_per_second"])
            log(
                f"    run {i + 1}/{runs}: pp {t['prompt_per_second']:.0f} t/s, "
                f"tg {t['predicted_per_second']:.1f} t/s"
            )
        short = complete_raw(server, "def main():\n", n_predict=256, cache_prompt=False)
        result.update(
            pp_tps=round(statistics.median(pp), 1),
            tg_at_depth_tps=round(statistics.median(tg), 2),
            tg_short_tps=round(short["timings"]["predicted_per_second"], 2),
            pp_runs=[round(x, 1) for x in pp],
            tg_runs=[round(x, 2) for x in tg],
            vram_peak_mib=vram_used_mib(),
            shared_mib=shared_gpu_mib(),
            server_ram_mib=process_ram_mib(server._proc.pid),
        )
    result["vram_baseline_mib"] = baseline_vram
    result["model_plus_ctx_mib"] = result["vram_peak_mib"] - baseline_vram
    result["spill_mib"] = max(0, result["shared_mib"] - baseline_shared)
    result["headroom_mib"] = GPU_TOTAL_MIB - result["vram_peak_mib"]
    return result


TOOLS = [
    {
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            },
        },
    }
    for name, desc in [
        ("read_file", "Read a file from the workspace."),
        ("list_dir", "List a directory in the workspace."),
        ("search_code", "Search the workspace for a regex."),
        ("run_tests", "Run the test suite for a path."),
        ("git_diff", "Show the git diff for a path."),
    ]
]


def prompt_cache(profile: Profile, log, prefix_tokens: int = 20_000, n: int = 10) -> dict:
    """B3: stable long prefix + varying suffix, with and without cache_prompt; tool reordering;
    and a growing multi-turn conversation (the agent-loop pattern)."""
    result: dict = {"prefix_tokens_target": prefix_tokens}
    with Server(profile, tag="b3") as server:
        prompt, n_prefix = build_prompt(server, prefix_tokens, {})
        result["prefix_tokens"] = n_prefix
        system = "You are a coding agent. Repository snapshot follows.\n\n" + prompt

        def ask(i: int, cache: bool, tools: list) -> dict:
            question = (
                f"Question {i}: name one file in the snapshot whose path contains "
                f"the letter {'aeiou'[i % 5]}."
            )
            msgs = [{"role": "system", "content": system}, {"role": "user", "content": question}]
            t = chat(
                server, msgs, reasoning="none", max_tokens=32, tools=tools, cache_prompt=cache
            )["timings"]
            return {
                "prompt_n": t["prompt_n"],
                "prompt_ms": round(t["prompt_ms"], 1),
                "cache_n": t.get("cache_n"),
            }

        for label, cache, reorder in [
            ("no_cache", False, False),
            ("cache", True, False),
            ("cache_tools_reordered", True, True),
        ]:
            rows = []
            for i in range(n):
                tools = TOOLS[i % 5 :] + TOOLS[: i % 5] if reorder else TOOLS
                rows.append(ask(i, cache, tools))
            steady = rows[1:]
            result[label] = {
                "rows": rows,
                "median_prompt_ms": statistics.median(r["prompt_ms"] for r in steady),
                "median_prompt_n": statistics.median(r["prompt_n"] for r in steady),
            }
            log(
                f"    {label}: median prompt {result[label]['median_prompt_ms']:.0f} ms, "
                f"{result[label]['median_prompt_n']} tokens processed"
            )
        nc, c = result["no_cache"]["median_prompt_ms"], result["cache"]["median_prompt_ms"]
        result["cache_speedup"] = round(nc / c, 1) if c else None

        # Agent loop: the conversation grows by a tool call + ~800-token result each turn.
        msgs = [
            {"role": "system", "content": system},
            {"role": "user", "content": "Inspect the snapshot and summarize its structure."},
        ]
        filler = repo_dump(200_000, seed=99)
        turns = []
        for turn in range(8):
            t = chat(server, msgs, reasoning="none", max_tokens=48, tools=TOOLS)
            msg = t["choices"][0]["message"]
            turns.append(
                {
                    "turn": turn,
                    "prompt_n": t["timings"]["prompt_n"],
                    "prompt_ms": round(t["timings"]["prompt_ms"], 1),
                }
            )
            call_id = f"call_{turn}"
            msgs.append(
                {
                    "role": "assistant",
                    "content": msg.get("content") or "",
                    "tool_calls": [
                        {
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": f'{{"path": "file_{turn}.rs"}}',
                            },
                        }
                    ],
                }
            )
            msgs.append({"role": "tool", "tool_call_id": call_id, "content": filler[turn][:2800]})
        result["agent_loop"] = turns
        log("    agent loop prompt_n per turn: " + ", ".join(str(t["prompt_n"]) for t in turns))
    return result


def reload_time(profile: Profile, log, repeats: int = 3) -> dict:
    """B8: time from process start to /health OK, repeated (warm file cache)."""
    times = []
    for i in range(repeats):
        start = time.monotonic()
        with Server(profile, tag=f"b8-{i}") as server:
            times.append(round(time.monotonic() - start, 2))
            first = chat(
                server, [{"role": "user", "content": "Say OK."}], reasoning="none", max_tokens=8
            )
        log(f"    load {i + 1}: {times[-1]} s")
    return {
        "load_seconds": times,
        "median": statistics.median(times),
        "first_prompt_ms": round(first["timings"]["prompt_ms"], 1),
    }
