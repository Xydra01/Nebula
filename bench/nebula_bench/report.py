"""Render results/<date>/raw/*.json into results/<date>/report.md."""

from __future__ import annotations

import json
import sys

from nebula_bench import toolcalls
from nebula_bench.run_all import OUT, RAW


def load(name: str) -> dict | None:
    path = RAW / f"{name}.json"
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else None


def table(header: list[str], rows: list[list]) -> list[str]:
    out = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    out += ["| " + " | ".join("" if c is None else str(c) for c in r) + " |" for r in rows]
    return out + [""]


def b1_section(models: list[str]) -> list[str]:
    lines = [
        "## B1/B2 throughput and VRAM",
        "",
        "Prompt filled to ~90% of the context window; pp = prompt processing, tg = "
        "generation. VRAM is whole-GPU usage including the ~1.4 GB desktop baseline.",
        "",
    ]
    rows = []
    for m in models:
        data = load(f"b1_{m}")
        if not data:
            continue
        for r in data["rows"]:
            note = r.get("skipped") or r.get("error") or ""
            rows.append(
                [
                    m,
                    r["kv_type"],
                    f"{r['ctx'] // 1024}K",
                    r.get("pp_tps"),
                    r.get("tg_at_depth_tps"),
                    r.get("tg_short_tps"),
                    r.get("vram_peak_mib"),
                    r.get("headroom_mib"),
                    r.get("spill_mib"),
                    r.get("server_ram_mib"),
                    r.get("load_seconds"),
                    note[:60],
                ]
            )
    return lines + table(
        [
            "model",
            "KV",
            "ctx",
            "pp t/s",
            "tg@depth",
            "tg short",
            "VRAM MiB",
            "headroom",
            "spill MiB",
            "server RAM MiB",
            "load s",
            "note",
        ],
        rows,
    )


def b7_section(models: list[str]) -> list[str]:
    lines = [
        "## B7 long-context needle retrieval",
        "",
        "easy: five distinct constants at 5/25/50/75/95% depth. hard: twenty near-identical "
        "names (SHARD_SALT_EU_WEST_2 vs _3) spread evenly. Haystack fills ~92% of ctx.",
        "",
    ]
    rows = []
    for m in models:
        for stage in (f"b7_{m}", f"b7hard_{m}"):
            data = load(stage)
            if not data:
                continue
            for r in data["rows"]:
                hits = "".join("Y" if a["correct"] else "n" for a in r.get("answers", []))
                acc = r.get("accuracy")
                rows.append(
                    [
                        m,
                        "hard" if "hard" in stage else "easy",
                        r["kv_type"],
                        "yes" if r.get("bias") else "no",
                        f"{r['ctx'] // 1024}K",
                        f"{acc * 100:.0f}%" if acc is not None else "",
                        hits,
                        r.get("skipped") or r.get("error", "")[:60],
                    ]
                )
    return lines + table(
        ["model", "test", "KV", "bias", "ctx", "accuracy", "by depth", "note"], rows
    )


def b3_section(models: list[str]) -> list[str]:
    lines = ["## B3 prompt cache (20K-token prefix)", ""]
    rows, loops = [], []
    for m in models:
        data = load(f"b3_{m}")
        if not data:
            continue
        rows += [
            [m, k, data[k]["median_prompt_ms"], data[k]["median_prompt_n"]]
            for k in ("no_cache", "cache", "cache_tools_reordered")
            if k in data
        ]
        loop = ", ".join(str(t["prompt_n"]) for t in data["agent_loop"])
        loops.append(
            f"- {m}: speedup **{data.get('cache_speedup')}x**; agent loop tokens "
            f"processed per turn: {loop}"
        )
    if not rows:
        return []
    return (
        lines
        + table(["model", "mode", "median prompt ms", "tokens processed"], rows)
        + loops
        + [""]
    )


def b8_section(models: list[str]) -> list[str]:
    rows = [
        [m, d["load_seconds"], d["median"], d["first_prompt_ms"]]
        for m in models
        if (d := load(f"b8_{m}"))
    ]
    return ["## B8 model load time", ""] + table(
        ["model", "loads (s)", "median s", "first prompt ms"], rows
    )


def b5_section(variants: list[str]) -> list[str]:
    rows, detail = [], []
    for v in variants:
        d = load(f"b5_{v}")
        if not d:
            continue
        toks = [r.get("completion_tokens") or 0 for r in d["rows"]]
        secs = sum(r["seconds"] for r in d["rows"])
        truncated = sum(r.get("finish") == "length" for r in d["rows"])
        rows.append(
            [
                v,
                f"{d['passed']}/{d['total']}",
                d["by_lang"]["python"],
                d["by_lang"]["rust"],
                d["by_lang"]["typescript"],
                round(sum(toks) / len(toks)),
                truncated,
                round(secs / 60, 1),
            ]
        )
        detail.append(
            f"- {v} failed: " + (", ".join(r["id"] for r in d["rows"] if not r["passed"]) or "none")
        )
    return (
        ["## B5 coding quality (20 tasks: 12 Python, 4 Rust, 4 TypeScript)", ""]
        + table(
            ["variant", "passed", "py", "rust", "ts", "avg tokens", "hit max_tokens", "minutes"],
            rows,
        )
        + detail
        + [""]
    )


def b6_section(models: list[str]) -> list[str]:
    rows, detail = [], []
    for m in models:
        d = load(f"b6_{m}")
        if not d:
            continue
        for mode, v in toolcalls.rescore(d)["modes"].items():
            s = v["summary"]
            rows.append(
                [
                    m,
                    mode,
                    s["valid"],
                    s["tool_ok"],
                    s["args_ok"],
                    v.get("summary_strict", s)["args_ok"],
                ]
            )
            detail.append(
                f"- {m} {mode} remaining failures: "
                + "; ".join(f"`{f['request'][:50]}` -> {f['got']}"[:140] for f in v["failures"][:6])
            )
    return [
        "## B6 tool calling (100 cases)",
        "",
        "valid = parseable output; tool = right tool(s) chosen; args = right tool and "
        "arguments. native = OpenAI tools API; prompted = JSON in text; schema = "
        "json_schema-constrained. Search patterns count as correct if the regex finds the "
        "intended text; 'args % (as run)' is the stricter scorer used during the run, which "
        "also had a backslash bug.",
        "",
        *table(["model", "mode", "valid %", "tool %", "args %", "args % (as run)"], rows),
        *detail,
        "",
    ]


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    bonsai = ["PTQ1_0", "PQ2_0"]
    fallback = ["ornith10", "ornith15", "deltacoder", "qwen36moe", "gemma4moe", "glm47flash"]
    variants = [f"{m}_{r}" for m in bonsai for r in ("none", "medium")] + [
        f"{m}_{r}" for m in fallback for r in ("none", "thinking")
    ]
    lines = [
        f"# Phase 0 WS3 benchmark report ({OUT.name})",
        "",
        "Generated by `nebula_bench.report` from `raw/*.json`. Hardware: RTX 4070 12 GB, "
        "i7-10700K, 32 GB RAM. Bonsai runs on PrismML llama.cpp b10743; fallback models "
        "on stock llama.cpp b11342. Decisions drawn from this report: "
        "[ADR-004](../../../docs/adr/ADR-004-model-profiles.md) and "
        "[ADR-005](../../../docs/adr/ADR-005-fallback-model.md).",
        "",
    ]
    lines += b1_section(bonsai + fallback)
    lines += b7_section(bonsai + fallback)
    lines += b3_section(["PTQ1_0", "qwen36moe", "gemma4moe"])
    lines += b8_section(bonsai)
    lines += b5_section(variants)
    lines += b6_section(bonsai + fallback)
    errors = sorted(p.name for p in RAW.glob("*.error.txt"))
    if errors:
        lines += ["## Failed stages", ""] + [f"- `{e}`" for e in errors] + [""]
    (OUT / "report.md").write_text("\n".join(lines), encoding="utf-8")
    print(f"wrote {OUT / 'report.md'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
