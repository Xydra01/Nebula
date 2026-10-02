"""Smoke checklist for a profile (PHASE0_PLAN task 3.6).

Checks: all layers offloaded, a sensible chat answer, a reasoning answer within budget,
a valid tool call, and an honored json_schema response format. Writes
results/<date>/smoke-<profile>.json and .md next to this package.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
from pathlib import Path

import httpx

from nebula_bench.client import chat as client_chat
from nebula_bench.client import vram_used_mib
from nebula_bench.server import Profile, Server

RESULTS_DIR = Path(__file__).resolve().parent.parent / "results"


def chat(server: Server, messages: list[dict], *, effort: str, max_tokens: int, **extra) -> dict:
    return client_chat(server, messages, reasoning=effort, max_tokens=max_tokens, **extra)


def check_offload(server: Server) -> tuple[bool, str]:
    m = re.search(r"offloaded (\d+)/(\d+) layers to GPU", server.log_text())
    if not m:
        return False, "no offload line in server log"
    done, total = map(int, m.groups())
    return done == total, f"{done}/{total} layers on GPU"


def check_auth(server: Server) -> tuple[bool, str]:
    url = f"{server.base_url}/v1/chat/completions"
    body = {"messages": [{"role": "user", "content": "hi"}], "max_tokens": 1}
    no_key = httpx.post(url, json=body, timeout=30).status_code
    preflight = httpx.options(
        url,
        headers={"Origin": "https://evil.example", "Access-Control-Request-Method": "POST"},
        timeout=30,
    )
    allowed = preflight.headers.get("access-control-allow-origin", "")
    ok = no_key == 401 and allowed in ("", "http://nebula.invalid")
    return ok, f"no-key status={no_key}, CORS allow-origin for evil.example={allowed!r}"


def check_chat(server: Server) -> tuple[bool, str, dict]:
    prompt = "What is 17 * 23? Reply with just the number."
    resp = chat(server, [{"role": "user", "content": prompt}], effort="none", max_tokens=256)
    content = resp["choices"][0]["message"].get("content") or ""
    return "391" in content, f"answer={content.strip()[:80]!r}", resp


def check_reasoning(server: Server) -> tuple[bool, str, dict]:
    resp = chat(
        server,
        [{"role": "user", "content": "Is 221 prime? Answer 'yes' or 'no' and one short reason."}],
        effort="medium",
        max_tokens=16384,
    )
    msg = resp["choices"][0]["message"]
    content = (msg.get("content") or "").strip()
    reasoning = msg.get("reasoning_content") or ""
    ok = bool(content) and "no" in content.lower()
    return (
        ok,
        (
            f"answer={content[:80]!r}, reasoning_chars={len(reasoning)}, "
            f"finish={resp['choices'][0].get('finish_reason')}"
        ),
        resp,
    )


WEATHER_TOOL = {
    "type": "function",
    "function": {
        "name": "get_weather",
        "description": "Get the current weather for a city.",
        "parameters": {
            "type": "object",
            "properties": {
                "city": {"type": "string"},
                "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]},
            },
            "required": ["city"],
        },
    },
}


def check_tool_call(server: Server) -> tuple[bool, str, dict]:
    resp = chat(
        server,
        [{"role": "user", "content": "What's the weather in Toronto in celsius?"}],
        effort="none",
        max_tokens=512,
        tools=[WEATHER_TOOL],
        tool_choice="auto",
    )
    calls = resp["choices"][0]["message"].get("tool_calls") or []
    if not calls:
        return False, "no tool_calls in response", resp
    fn = calls[0]["function"]
    try:
        args = json.loads(fn["arguments"])
    except (json.JSONDecodeError, TypeError):
        return False, f"arguments not JSON: {fn.get('arguments')!r}", resp
    ok = fn["name"] == "get_weather" and "toronto" in str(args.get("city", "")).lower()
    return ok, f"{fn['name']}({json.dumps(args)})", resp


PRIMES_SCHEMA = {
    "type": "object",
    "properties": {
        "language": {"type": "string"},
        "primes": {"type": "array", "items": {"type": "integer"}, "minItems": 5, "maxItems": 5},
    },
    "required": ["language", "primes"],
    "additionalProperties": False,
}


def check_json_schema(server: Server) -> tuple[bool, str, dict]:
    resp = chat(
        server,
        [{"role": "user", "content": "Give the first five primes, and name the language Rust."}],
        effort="none",
        max_tokens=512,
        response_format={
            "type": "json_schema",
            "json_schema": {"name": "primes", "schema": PRIMES_SCHEMA, "strict": True},
        },
    )
    content = resp["choices"][0]["message"].get("content") or ""
    try:
        data = json.loads(content)
    except json.JSONDecodeError:
        return False, f"not JSON: {content[:80]!r}", resp
    ok = set(data) == {"language", "primes"} and data["primes"] == [2, 3, 5, 7, 11]
    return ok, f"parsed={json.dumps(data)}", resp


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", default="standard")
    args = parser.parse_args()

    profile = Profile.load(args.profile)
    baseline = vram_used_mib()
    results: dict = {
        "profile": args.profile,
        "date": time.strftime("%Y-%m-%d %H:%M"),
        "vram_baseline_mib": baseline,
        "checks": {},
    }

    with Server(profile) as server:
        results["load_seconds"] = round(server.load_seconds, 1)
        results["vram_loaded_mib"] = vram_used_mib()
        ok, detail = check_offload(server)
        results["checks"]["offload"] = {"ok": ok, "detail": detail}
        ok, detail = check_auth(server)
        results["checks"]["auth"] = {"ok": ok, "detail": detail}
        for name, fn in [
            ("chat", check_chat),
            ("reasoning", check_reasoning),
            ("tool_call", check_tool_call),
            ("json_schema", check_json_schema),
        ]:
            try:
                ok, detail, resp = fn(server)
                results["checks"][name] = {
                    "ok": ok,
                    "detail": detail,
                    "timings": resp.get("timings"),
                    "usage": resp.get("usage"),
                }
            except Exception as e:  # noqa: BLE001 - every failure is a reportable result
                results["checks"][name] = {"ok": False, "detail": f"error: {e}"}
        results["vram_peak_mib"] = vram_used_mib()
        results["server_log"] = str(server.log_path)

    out_dir = RESULTS_DIR / time.strftime("%Y-%m-%d")
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / f"smoke-{args.profile}.json").write_text(
        json.dumps(results, indent=2, ensure_ascii=False), encoding="utf-8"
    )

    lines = [
        f"# Smoke test: `{args.profile}` ({results['date']})",
        "",
        f"- Load time: {results['load_seconds']} s",
        f"- VRAM: baseline {baseline} MiB, loaded {results['vram_loaded_mib']} MiB, "
        f"after checks {results['vram_peak_mib']} MiB "
        f"(model + context = {results['vram_peak_mib'] - baseline} MiB)",
        "",
        "| Check | Result | Detail | Gen t/s |",
        "| --- | --- | --- | --- |",
    ]
    for name, c in results["checks"].items():
        tps = (c.get("timings") or {}).get("predicted_per_second")
        lines.append(
            f"| {name} | {'PASS' if c['ok'] else 'FAIL'} | {c['detail']} | "
            f"{f'{tps:.1f}' if tps else ''} |"
        )
    report = "\n".join(lines) + "\n"
    (out_dir / f"smoke-{args.profile}.md").write_text(report, encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    print(report)
    return 0 if all(c["ok"] for c in results["checks"].values()) else 1


if __name__ == "__main__":
    sys.exit(main())
