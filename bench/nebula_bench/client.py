"""HTTP client helpers and GPU memory readings shared by the benchmarks."""

from __future__ import annotations

import subprocess

import httpx

from nebula_bench.server import Server

GPU_TOTAL_MIB = 12282


def chat(
    server: Server,
    messages: list[dict],
    *,
    reasoning: str = "none",
    max_tokens: int = 1024,
    timeout: float = 1800,
    **extra,
) -> dict:
    """One chat completion. `reasoning` is none|low|medium|xhigh (Bonsai) or none|on (Qwen)."""
    profile = server.profile
    body: dict = {"messages": messages, "max_tokens": max_tokens, "cache_prompt": True}
    if profile.reasoning_style == "effort":
        body["reasoning_effort"] = reasoning
        body["chat_template_kwargs"] = {"reasoning_effort": reasoning}
    else:
        body["chat_template_kwargs"] = {"enable_thinking": reasoning != "none"}
    preset = "instruct" if reasoning == "none" else "thinking"
    body.update(profile.sampling.get(preset, {}))
    body.update(extra)
    r = httpx.post(
        f"{server.base_url}/v1/chat/completions", json=body, headers=server.headers, timeout=timeout
    )
    if r.status_code != 200:
        raise RuntimeError(f"HTTP {r.status_code}: {r.text[:500]}")
    return r.json()


def complete_raw(server: Server, prompt: str, *, n_predict: int, cache_prompt: bool) -> dict:
    """Raw /completion call (no chat template), used for throughput measurements."""
    body = {
        "prompt": prompt,
        "n_predict": n_predict,
        "cache_prompt": cache_prompt,
        "temperature": 0.0,
        "ignore_eos": True,
    }
    r = httpx.post(f"{server.base_url}/completion", json=body, headers=server.headers, timeout=3600)
    if r.status_code != 200:
        raise RuntimeError(f"HTTP {r.status_code}: {r.text[:500]}")
    return r.json()


def count_tokens(server: Server, text: str) -> int:
    r = httpx.post(
        f"{server.base_url}/tokenize", json={"content": text}, headers=server.headers, timeout=600
    )
    r.raise_for_status()
    return len(r.json()["tokens"])


def vram_used_mib() -> int:
    out = subprocess.run(
        ["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return int(out.strip().splitlines()[0])


def process_ram_mib(pid: int) -> int:
    """Private bytes of a process; for MoE offload this is where the expert weights live."""
    cmd = f"[int]((Get-Process -Id {pid}).PrivateMemorySize64/1MB)"
    out = subprocess.run(
        ["powershell", "-NoProfile", "-Command", cmd], capture_output=True, text=True, timeout=60
    ).stdout.strip()
    return int(out) if out.isdigit() else -1


def shared_gpu_mib() -> int:
    """Largest 'Shared Usage' across GPU adapters: VRAM that spilled into system RAM."""
    cmd = (
        "(Get-Counter '\\GPU Adapter Memory(*)\\Shared Usage').CounterSamples | "
        "Measure-Object -Property CookedValue -Maximum | ForEach-Object { [int]($_.Maximum/1MB) }"
    )
    out = subprocess.run(
        ["powershell", "-NoProfile", "-Command", cmd], capture_output=True, text=True, timeout=60
    ).stdout.strip()
    return int(out) if out.isdigit() else -1
