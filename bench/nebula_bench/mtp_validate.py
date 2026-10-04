"""Validation of the PQ2_0+MTP profile picked by the B4 sweep: throughput and VRAM per KV
configuration, tool calls, coding tasks, prompt cache and the hard needle test.

    uv run python -m nebula_bench.mtp_validate
"""

from __future__ import annotations

import ctypes
import json
from collections.abc import Callable
from pathlib import Path

from nebula_bench import needle, perf, quality, toolcalls
from nebula_bench.mtp import RAW, log
from nebula_bench.server import Profile

B1_CONFIGS = [("f16", 32768), ("q8_0", 32768), ("q4_0", 32768), ("q4_0", 65536), ("q4_0", 131072)]


def model_buffer_mib(p: Profile) -> int:
    return int(Path(p.model).stat().st_size / 2**20) - 260


def b1(p: Profile) -> dict:
    rows = []
    for kv, ctx in B1_CONFIGS:
        log(f"  B1 {p.name} {kv} {ctx // 1024}K")
        try:
            rows.append(perf.throughput(p, ctx, kv, model_buffer_mib(p), log))
        except Exception as e:  # noqa: BLE001
            rows.append({"ctx": ctx, "kv_type": kv, "error": repr(e)[:400]})
            log(f"    error: {e!r}"[:300])
    return {"model": p.model, "rows": rows}


def b7hard(p: Profile) -> dict:
    rows = []
    for kv, ctx in [("q4_0", 32768), ("q4_0", 65536)]:
        try:
            rows.append(needle.run(p, ctx, kv, None, log, hard=True))
        except Exception as e:  # noqa: BLE001
            rows.append({"ctx": ctx, "kv_type": kv, "error": repr(e)[:400]})
    return {"model": p.model, "rows": rows}


def without_mtp(p: Profile) -> Profile:
    flags = list(p.flags)
    i = flags.index("--spec-type")
    del flags[i : i + 4]
    return p.with_(name="pq2nomtp", flags=flags)


def long_no_cache_ram(p: Profile) -> dict:
    flags = list(p.flags)
    i = flags.index("--cache-ram")
    flags[i + 1] = "0"
    q = p.with_(name="pq2mtp-cr0", flags=flags)
    row = perf.throughput(q, 131072, "q4_0", model_buffer_mib(q), log)
    return {"model": p.model, "rows": [row]}


def stages(p: Profile) -> list[tuple[str, Callable[[], dict]]]:
    off = without_mtp(p)
    return [
        ("v_b6_pq2mtp", lambda: toolcalls.run(p, ["native", "schema"], log)),
        ("v_b5_pq2mtp_none", lambda: quality.run(p, "none", 4096, log)),
        ("v_b3_pq2mtp", lambda: perf.prompt_cache(p, log, n=5)),
        ("v_b1_pq2mtp", lambda: b1(p)),
        ("v_b7hard_pq2mtp", lambda: b7hard(p)),
        ("v_b5_pq2mtp_medium", lambda: quality.run(p, "medium", 16384, log)),
        # Second samples: one 20-task run moves by 2-3 tasks on sampling noise alone.
        ("v_b5_pq2mtp_medium_r2", lambda: quality.run(p, "medium", 16384, log)),
        ("v_b5_pq2mtp_none_r2", lambda: quality.run(p, "none", 4096, log)),
        # Same-day control: the same file and harness with drafting off.
        ("v_b5_pq2nomtp_medium", lambda: quality.run(off, "medium", 16384, log)),
        ("v_b5_pq2nomtp_medium_r2", lambda: quality.run(off, "medium", 16384, log)),
        ("v_b1_pq2mtp_128k_cr0", lambda: long_no_cache_ram(p)),
    ]


def main() -> None:
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED = 0x80000000, 0x00000001
    ctypes.windll.kernel32.SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)
    RAW.mkdir(parents=True, exist_ok=True)
    p = Profile.load("pq2mtp")
    for name, fn in stages(p):
        out = RAW / f"{name}.json"
        if out.exists():
            log(f"skip {name} (done)")
            continue
        log(f"=== {name}")
        try:
            res = fn()
        except Exception as e:  # noqa: BLE001
            res = {"error": repr(e)}
            log(f"    ERROR {e!r}")
        out.write_text(json.dumps(res, indent=2), encoding="utf-8")
    log("=== validation done ===")


if __name__ == "__main__":
    main()
