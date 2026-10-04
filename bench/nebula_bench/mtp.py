"""B4: MTP speculative decoding on Bonsai 2 27B (PQ2_0 with the ProCreations head, and PTQ1_0
with the same head grafted on).

For each model, the same GGUF is served with `--spec-type none` and with `draft-mtp` at several
`--spec-draft-n-max` values, and a fixed set of chat workloads is timed with the profile's own
sampling presets. Results are resumable per (model, config).

    uv run python -m nebula_bench.mtp
"""

from __future__ import annotations

import ctypes
import json
import time
from pathlib import Path

from nebula_bench.client import GPU_TOTAL_MIB, chat, vram_used_mib
from nebula_bench.perf import build_prompt
from nebula_bench.server import Profile, Server

RUN_DATE = "2026-10-03"
OUT = Path(__file__).resolve().parent.parent / "results" / RUN_DATE
RAW = OUT / "raw"
MTP_DIR = Path(r"F:\Nebula\models\bonsai2-27b-mtp")
MODELS = {
    "PQ2_0+MTP": MTP_DIR / "Ternary-Bonsai-2-27B-PQ2_0-MTP-Q8_0.gguf",
    "PTQ1_0+MTP": MTP_DIR / "Ternary-Bonsai-2-27B-PTQ1_0-MTP-Q8_0.gguf",
}
DRAFT_N = [None, 1, 2, 3, 4, 5]
REPEATS = 2
EDIT_SOURCE = Path(__file__).resolve().parent / "server.py"


def log(msg: str) -> None:
    line = f"{time.strftime('%H:%M:%S')} {msg}"
    print(line, flush=True)
    OUT.mkdir(parents=True, exist_ok=True)
    with (OUT / "progress-mtp.log").open("a", encoding="utf-8") as f:
        f.write(line + "\n")


def workloads(server: Server, cache: dict) -> list[dict]:
    if "long" not in cache:
        cache["long"] = build_prompt(server, 20_000, {})[0]
    source = EDIT_SOURCE.read_text(encoding="utf-8")
    return [
        {
            "name": "code_write",
            "reasoning": "none",
            "max_tokens": 600,
            "messages": [
                {
                    "role": "user",
                    "content": "Write a Python module implementing an LRU cache class with get, "
                    "put and a max size, fully type-hinted with docstrings, followed by pytest "
                    "tests.",
                }
            ],
        },
        {
            "name": "code_edit",
            "reasoning": "none",
            "max_tokens": 1500,
            "messages": [
                {
                    "role": "user",
                    "content": "Return this whole file unchanged except: rename the class `Server` "
                    "to `LlamaServer` everywhere. Output only the file in one code block.\n\n"
                    f"```python\n{source}\n```",
                }
            ],
        },
        {
            "name": "json",
            "reasoning": "none",
            "max_tokens": 400,
            "messages": [
                {
                    "role": "user",
                    "content": "Output a JSON array of 8 objects describing fictional git commits, "
                    "each with sha, author, date, message and files_changed (array of paths). "
                    "Only JSON.",
                }
            ],
        },
        {
            "name": "prose",
            "reasoning": "none",
            "max_tokens": 500,
            "messages": [
                {
                    "role": "user",
                    "content": "In plain prose with no lists, explain to a new engineer why "
                    "speculative decoding speeds up language model inference and when it does not.",
                }
            ],
        },
        {
            "name": "reasoning_medium",
            "reasoning": "medium",
            "max_tokens": 1500,
            "messages": [
                {
                    "role": "user",
                    "content": "A Rust function must merge overlapping half-open intervals given "
                    "as Vec<(i64, i64)>. Think through the edge cases, then write it.",
                }
            ],
        },
        {
            "name": "long_ctx",
            "reasoning": "none",
            "max_tokens": 400,
            "messages": [
                {
                    "role": "system",
                    "content": "You are a coding agent. Repository snapshot follows.\n\n"
                    + cache["long"],
                },
                {
                    "role": "user",
                    "content": "Summarize what the benchmark harness in this snapshot does, "
                    "module by module.",
                },
            ],
        },
    ]


def run_config(model: str, kv_type: str, draft_n: int | None) -> dict:
    base = Profile.load("standard")
    flags = list(base.flags)
    if draft_n is not None:
        flags += ["--spec-type", "draft-mtp", "--spec-draft-n-max", str(draft_n)]
    profile = base.with_(
        name=f"mtp-{model}", model=str(MODELS[model]), kv_type=kv_type, flags=flags
    )
    tag = f"{kv_type}-n{draft_n or 0}"
    result: dict = {"model": model, "kv_type": kv_type, "draft_n": draft_n}
    baseline = vram_used_mib()
    with Server(profile, tag=tag) as server:
        result["load_seconds"] = round(server.load_seconds, 2)
        text = server.log_text()
        result["mtp_context"] = "draft context" in text
        cache: dict = {}
        rows = []
        for w in workloads(server, cache):
            for rep in range(REPEATS):
                r = chat(
                    server,
                    w["messages"],
                    reasoning=w["reasoning"],
                    max_tokens=w["max_tokens"],
                    cache_prompt=False,
                )
                t = r["timings"]
                msg = r["choices"][0]["message"]
                rows.append(
                    {
                        "workload": w["name"],
                        "rep": rep,
                        "predicted_n": t["predicted_n"],
                        "predicted_ms": round(t["predicted_ms"], 1),
                        "tps": round(t["predicted_per_second"], 2),
                        "prompt_n": t["prompt_n"],
                        "prompt_tps": round(t["prompt_per_second"], 1),
                        "draft_n": t.get("draft_n"),
                        "draft_accepted": t.get("draft_n_accepted"),
                        "sample": (msg.get("content") or "")[:300],
                    }
                )
                log(
                    f"    {w['name']} #{rep}: {t['predicted_per_second']:.1f} t/s, "
                    f"{t['predicted_n']} tok, draft {t.get('draft_n')}/{t.get('draft_n_accepted')}"
                )
        result["rows"] = rows
        result["vram_peak_mib"] = vram_used_mib()
    result["vram_baseline_mib"] = baseline
    result["headroom_mib"] = GPU_TOTAL_MIB - result["vram_peak_mib"]
    return result


def main() -> None:
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED = 0x80000000, 0x00000001
    ctypes.windll.kernel32.SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)
    RAW.mkdir(parents=True, exist_ok=True)
    plan = [("PQ2_0+MTP", "q4_0"), ("PTQ1_0+MTP", "q4_0"), ("PTQ1_0+MTP", "f16")]
    for model, kv in plan:
        if not MODELS[model].exists():
            log(f"skip {model}: {MODELS[model]} missing")
            continue
        for n in DRAFT_N:
            out = RAW / f"mtp-{model.replace('+', '_')}-{kv}-n{n or 0}.json"
            if out.exists():
                log(f"skip {out.name} (done)")
                continue
            log(f"=== {model} kv={kv} draft_n={n}")
            try:
                res = run_config(model, kv, n)
            except Exception as e:  # noqa: BLE001 - record and continue with the next config
                res = {"model": model, "kv_type": kv, "draft_n": n, "error": repr(e)}
                log(f"    ERROR {e!r}")
            out.write_text(json.dumps(res, indent=2), encoding="utf-8")
    log("=== MTP run done ===")


if __name__ == "__main__":
    main()
