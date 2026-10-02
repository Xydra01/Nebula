"""Unattended overnight run of the Phase 0 benchmark matrix.

Each stage writes results/<date>/raw/<stage>.json as soon as it finishes; re-running skips
finished stages, so a crash or reboot loses at most one stage. Stages run in priority order
(ADR-004 inputs first, then fallback models as their downloads complete).
"""

from __future__ import annotations

import ctypes
import hashlib
import json
import sys
import time
import traceback
from collections.abc import Callable
from pathlib import Path

from nebula_bench import needle, perf, quality, toolcalls
from nebula_bench.server import Profile

RUN_DATE = "2026-10-02"
OUT = Path(__file__).resolve().parent.parent / "results" / RUN_DATE
RAW = OUT / "raw"
MODELS = Path(r"F:\Nebula\models\bonsai2-27b")
BIAS = {q: str(MODELS / f"Ternary-Bonsai-2-27B-{q}-kv-bias.gguf") for q in ("PTQ1_0", "PQ2_0")}
FALLBACK_SHA = {
    "ornith10": "33b6f6a3e3f05078438e12df8a4b55c8acf78ceadcc639d2af1cf35a026e8387",
    "ornith15": "b6f76e74f86245b3caee014b797c10dca931c4dfdaabfb134eab655f81e4154a",
    "deltacoder": "0f57f0cda3cb1e027e947ce6e8f15f5743f150ec8cd28f29dacde0b8748bf3cd",
}
# Round 2: MoE models with routed experts in system RAM (`--fit on`); the 9B round was too weak.
MOE_SHA = {
    "qwen36moe": "707a55a8a4397ecde44de0c499d3e68c1ad1d240d1da65826b4949d1043f4450",
    "gemma4moe": "ef728c8e0c337fd1067b947af006e38a9ef2419e56feced4fd29b4bf0636e30c",
    "glm47flash": "b0d4fbc1211f891b4cfbf2a497160bfe06a49412420068904d426b7a13f4ba7f",
}
MOE_BYTES = {"qwen36moe": 22360456160, "gemma4moe": 17010980576, "glm47flash": 17520169312}
ALL_SHA = FALLBACK_SHA | MOE_SHA
ES_CONTINUOUS, ES_SYSTEM_REQUIRED = 0x80000000, 0x00000001


def log(msg: str) -> None:
    line = f"{time.strftime('%H:%M:%S')} {msg}"
    print(line, flush=True)
    with (OUT / "progress.log").open("a", encoding="utf-8") as f:
        f.write(line + "\n")


def bonsai(quant: str) -> Profile:
    return Profile.load("standard").with_(
        name=f"bonsai-{quant}", model=str(MODELS / f"Ternary-Bonsai-2-27B-{quant}.gguf")
    )


def model_buffer_mib(p: Profile) -> int:
    return int(Path(p.model).stat().st_size / 2**20) - 260


def b1(p: Profile) -> dict:
    rows = []
    for kv in ("f16", "q4_0"):
        for ctx in (8192, 32768, 65536, 131072):
            log(f"  B1 {p.name} {kv} {ctx // 1024}K")
            try:
                rows.append(perf.throughput(p, ctx, kv, model_buffer_mib(p), log))
            except Exception as e:  # noqa: BLE001 - one config failing must not stop the rest
                rows.append({"ctx": ctx, "kv_type": kv, "error": repr(e)[:400]})
                log(f"    error: {e!r}"[:300])
    return {"model": p.model, "rows": rows}


def b7(p: Profile, quant: str, configs: list[tuple[str, bool, int]], hard: bool = False) -> dict:
    rows = []
    for kv, use_bias, ctx in configs:
        if not perf.fits(model_buffer_mib(p), ctx, kv)[0]:
            rows.append({"ctx": ctx, "kv_type": kv, "bias": use_bias, "skipped": "does not fit"})
            continue
        try:
            rows.append(needle.run(p, ctx, kv, BIAS[quant] if use_bias else None, log, hard))
        except Exception as e:  # noqa: BLE001
            rows.append({"ctx": ctx, "kv_type": kv, "bias": use_bias, "error": repr(e)[:400]})
            log(f"    error: {e!r}"[:300])
    return {"model": p.model, "rows": rows}


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        while chunk := f.read(16 * 2**20):
            h.update(chunk)
    return h.hexdigest()


def fallback_ready(name: str, verified: dict) -> bool:
    if name in verified:
        return verified[name]
    path = Path(Profile.load(name).model)
    if not path.exists():
        return False
    expected = MOE_BYTES.get(name)
    size = path.stat().st_size
    if expected is not None and size != expected:
        return False  # still downloading
    time.sleep(20)
    if path.stat().st_size != size or size < 6 * 2**30:
        return False  # still downloading
    verified[name] = sha256(path) == ALL_SHA[name]
    log(f"  {name}: download complete, hash {'OK' if verified[name] else 'MISMATCH'}")
    return verified[name]


Stage = tuple[str, str | None, Callable[[], dict]]  # (name, fallback model it needs, fn)


def stages() -> list[Stage]:
    """Priority order: ADR-004 essentials, then cheap fallback runs, then long reasoning runs."""
    ptq, pq2 = bonsai("PTQ1_0"), bonsai("PQ2_0")
    b7_full = [
        ("f16", False, 32768),
        ("f16", False, 65536),
        ("q4_0", False, 32768),
        ("q4_0", False, 65536),
        ("q4_0", False, 131072),
        ("q4_0", True, 32768),
        ("q4_0", True, 65536),
        ("q4_0", True, 131072),
    ]
    out: list[Stage] = [
        ("b1_PTQ1_0", None, lambda: b1(ptq)),
        ("b7_PTQ1_0", None, lambda: b7(ptq, "PTQ1_0", b7_full)),
        ("b3_PTQ1_0", None, lambda: perf.prompt_cache(ptq, log)),
        ("b6_PTQ1_0", None, lambda: toolcalls.run(ptq, ["native", "prompted", "schema"], log)),
        ("b5_PTQ1_0_none", None, lambda: quality.run(ptq, "none", 4096, log)),
        ("b8_PTQ1_0", None, lambda: perf.reload_time(ptq, log)),
        ("b8_PQ2_0", None, lambda: perf.reload_time(pq2, log)),
        ("b1_PQ2_0", None, lambda: b1(pq2)),
        ("b6_PQ2_0", None, lambda: toolcalls.run(pq2, ["native", "schema"], log)),
        ("b5_PQ2_0_none", None, lambda: quality.run(pq2, "none", 4096, log)),
        (
            "b7hard_PTQ1_0",
            None,
            lambda: b7(
                ptq,
                "PTQ1_0",
                [
                    ("f16", False, 65536),
                    ("q4_0", False, 65536),
                    ("q4_0", True, 65536),
                    ("q4_0", False, 131072),
                    ("q4_0", True, 131072),
                ],
                hard=True,
            ),
        ),
        ("b5_PTQ1_0_medium", None, lambda: quality.run(ptq, "medium", 16384, log)),
    ]
    for name in FALLBACK_SHA:
        p = Profile.load(name)
        out += [
            (f"b6_{name}", name, lambda p=p: toolcalls.run(p, ["native", "schema"], log)),
            (f"b5_{name}_none", name, lambda p=p: quality.run(p, "none", 4096, log)),
            (
                f"b1_{name}",
                name,
                lambda p=p: {
                    "model": p.model,
                    "rows": [perf.throughput(p, 32768, "f16", model_buffer_mib(p), log)],
                },
            ),
        ]
    out += [
        (f"b5_{n}_thinking", n, lambda n=n: quality.run(Profile.load(n), "on", 16384, log))
        for n in FALLBACK_SHA
    ]
    out += [
        ("b5_PQ2_0_medium", None, lambda: quality.run(pq2, "medium", 16384, log)),
        (
            "b7_PQ2_0",
            None,
            lambda: b7(pq2, "PQ2_0", [("q4_0", True, 65536), ("q4_0", True, 131072)]),
        ),
    ]
    for name in MOE_SHA:
        p = Profile.load(name)
        out += [
            # `--fit` decides GPU placement, so the VRAM pre-check is bypassed (buffer 0).
            (
                f"b1_{name}",
                name,
                lambda p=p: {
                    "model": p.model,
                    "rows": [
                        perf.throughput(p, 32768, "f16", 0, log),
                        perf.throughput(p, 131072, "q8_0", 0, log),
                    ],
                },
            ),
            (f"b6_{name}", name, lambda p=p: toolcalls.run(p, ["native", "schema"], log)),
            (f"b5_{name}_none", name, lambda p=p: quality.run(p, "none", 4096, log)),
            (f"b5_{name}_thinking", name, lambda p=p: quality.run(p, "on", 16384, log)),
            (
                f"b7hard_{name}",
                name,
                lambda p=p: {
                    "model": p.model,
                    "rows": [needle.run(p, 32768, "f16", None, log, hard=True)],
                },
            ),
        ]
    # The two finalists: does the agent-loop prompt cache work on them?
    out += [
        (f"b3_{n}", n, lambda n=n: perf.prompt_cache(Profile.load(n), log, n=5))
        for n in ("qwen36moe", "gemma4moe")
    ]
    return out


def run_stage(name: str, fn: Callable[[], dict]) -> None:
    out = RAW / f"{name}.json"
    if out.exists():
        log(f"skip {name} (done)")
        return
    log(f"START {name}")
    start = time.monotonic()
    try:
        result = fn()
        result["stage_seconds"] = round(time.monotonic() - start)
        out.write_text(json.dumps(result, indent=1, ensure_ascii=False), encoding="utf-8")
        log(f"DONE  {name} in {result['stage_seconds'] / 60:.1f} min")
    except Exception as e:  # noqa: BLE001 - record and continue with the next stage
        (RAW / f"{name}.error.txt").write_text(traceback.format_exc(), encoding="utf-8")
        log(f"FAIL  {name}: {e!r}"[:400])


def main() -> int:
    RAW.mkdir(parents=True, exist_ok=True)
    ctypes.windll.kernel32.SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)
    log("=== overnight run starting ===")
    bad = quality.selfcheck()
    if bad:
        log(f"reference self-check failed for {bad}; aborting")
        return 1
    verified: dict = {}
    deferred: list[Stage] = []
    for stage in stages():
        name, needs, fn = stage
        if needs and not fallback_ready(needs, verified):
            if verified.get(needs) is False:
                log(f"skip {name}: {needs} hash mismatch")
            else:
                deferred.append(stage)
            continue
        run_stage(name, fn)

    deadline = time.monotonic() + 8 * 3600
    while deferred and time.monotonic() < deadline:
        for stage in list(deferred):
            name, needs, fn = stage
            if fallback_ready(needs, verified):
                run_stage(name, fn)
                deferred.remove(stage)
            elif verified.get(needs) is False:
                deferred.remove(stage)
        if deferred:
            time.sleep(60)
    if deferred:
        log(f"gave up waiting for downloads: {[s[0] for s in deferred]}")
    log("=== overnight run finished ===")
    return 0


if __name__ == "__main__":
    sys.exit(main())
