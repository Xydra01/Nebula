"""B7: needle-in-a-repo retrieval at long context.

Five constants are planted as small fake files at 5/25/50/75/95% depth of a repo dump.
The haystack is sent once as a stable prefix; each question differs only at the end, so after
the first question the prefix comes from the prompt cache.
"""

from __future__ import annotations

import random

from nebula_bench.client import chat, count_tokens
from nebula_bench.perf import build_prompt
from nebula_bench.server import Profile, Server

NEEDLES = [
    (0.05, "src/release/codename.rs", "RELEASE_CODENAME", "amber-falcon-4127"),
    (0.25, "nebula/limits.py", "MAX_PARALLEL_FETCHES", "37"),
    (0.50, "src/net/endpoints.rs", "TELEMETRY_SINK_PORT", "48211"),
    (0.75, "config/owners.py", "ON_CALL_ALIAS", "violet-heron"),
    (0.95, "src/storage/shards.rs", "SHARD_SALT", "q7k2-m9x4"),
]


def _needle_file(path: str, name: str, value: str) -> str:
    if path.endswith(".rs"):
        body = f'/// Set by the release process.\npub const {name}: &str = "{value}";\n'
    else:
        body = f'# Set by the release process.\n{name} = "{value}"\n'
    return f"// FILE: {path}\n{body}\n"


def hard_needles(n: int = 20, seed: int = 11) -> list[tuple[float, str, str, str]]:
    """Near-identical names (SHARD_SALT_EU_WEST_2 vs SHARD_SALT_EU_WEST_3) spread evenly."""
    rng = random.Random(seed)
    regions = ["EU_WEST", "EU_EAST", "US_WEST", "US_EAST", "AP_SOUTH"]
    alphabet = "abcdefghjkmnpqrstuvwxyz23456789"
    out = []
    for i in range(n):
        name = f"SHARD_SALT_{regions[i % 5]}_{i // 5 + 1}"
        value = (
            "".join(rng.choice(alphabet) for _ in range(4))
            + "-"
            + "".join(rng.choice(alphabet) for _ in range(4))
        )
        out.append(((i + 0.5) / n, f"src/storage/salts_{i:02d}.rs", name, value))
    return out


def haystack(server: Server, target_tokens: int, needles=NEEDLES) -> tuple[str, int]:
    text, _ = build_prompt(server, target_tokens - 40 * len(needles) - 200, {})
    lines = text.splitlines(keepends=True)
    for depth, path, name, value in sorted(needles, reverse=True):
        at = int(len(lines) * depth)
        while at < len(lines) and not lines[at].startswith("// FILE:"):
            at += 1
        lines.insert(at, _needle_file(path, name, value))
    full = "".join(lines)
    return full, count_tokens(server, full)


def run(
    profile: Profile, ctx: int, kv_type: str, kv_bias: str | None, log, hard: bool = False
) -> dict:
    needles = hard_needles() if hard else NEEDLES
    p = profile.with_(ctx=ctx, kv_type=kv_type, kv_bias=kv_bias)
    tag = f"b7{'hard' if hard else ''}-{kv_type}{'-bias' if kv_bias else ''}-{ctx // 1024}k"
    result: dict = {
        "ctx": ctx,
        "kv_type": kv_type,
        "bias": bool(kv_bias),
        "hard": hard,
        "answers": [],
    }
    with Server(p, tag=tag) as server:
        text, n = haystack(server, int(ctx * 0.92), needles)
        result["haystack_tokens"] = n
        system = (
            "You are a coding agent. The full repository snapshot follows. Answer questions "
            "about it exactly.\n\n" + text
        )
        correct = 0
        for depth, _path, name, value in needles:
            msgs = [
                {"role": "system", "content": system},
                {
                    "role": "user",
                    "content": f"What is the value of the constant {name} "
                    f"defined in the snapshot? Reply with the value "
                    f"only.",
                },
            ]
            resp = chat(server, msgs, reasoning="none", max_tokens=48)
            answer = (resp["choices"][0]["message"].get("content") or "").strip()
            hit = value in answer
            correct += hit
            result["answers"].append(
                {
                    "depth": depth,
                    "name": name,
                    "expected": value,
                    "answer": answer[:80],
                    "correct": hit,
                    "prompt_ms": round(resp["timings"]["prompt_ms"], 1),
                }
            )
        result["accuracy"] = correct / len(needles)
        log(f"    {tag}: {correct}/{len(needles)} correct ({n} tokens)")
    return result
