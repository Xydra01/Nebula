"""PHASE0_PLAN 3.5: the CPU embedding server.

Checks that Qwen3-Embedding-0.6B loads on the CPU, measures its memory and throughput, runs a
retrieval sanity test over this repo's own functions, and measures whether CPU embedding slows the
`standard` chat model's generation on the GPU.

Run: uv run python -m nebula_bench.embed
"""

from __future__ import annotations

import ast
import json
import math
import threading
import time
from pathlib import Path

import httpx

from nebula_bench.client import complete_raw, process_ram_mib, vram_used_mib
from nebula_bench.server import Profile, Server

RUN_DATE = "2026-10-04"
OUT_DIR = Path(__file__).resolve().parent.parent / "results" / RUN_DATE
SRC_DIR = Path(__file__).resolve().parent
QUERY_TASK = "Given a description of what some code does, retrieve the Python function that does it"

# Query -> qualified name of the function that answers it.
QUERIES = {
    "start llama-server as a child process and wait until its health endpoint answers": (
        "Server._wait_healthy"
    ),
    "pick an unused TCP port on localhost": "free_port",
    "read the key-value metadata and tensor table from a GGUF file": "read_header",
    "copy the MTP head tensors from one model file into another model file": "graft",
    "ask nvidia-smi how much GPU memory is in use": "vram_used_mib",
    "count how many tokens a string is": "count_tokens",
    "private memory of a process in MiB using PowerShell": "process_ram_mib",
    "send a chat completion request with a reasoning effort setting": "chat",
    "spilled VRAM that went into shared system memory": "shared_gpu_mib",
    "load a server profile from a TOML file": "Profile.load",
}


def code_chunks() -> dict[str, str]:
    """Every top-level function and method in nebula_bench, keyed by qualified name."""
    chunks: dict[str, str] = {}
    for path in sorted(SRC_DIR.glob("*.py")):
        text = path.read_text(encoding="utf-8")
        tree = ast.parse(text)
        for node in tree.body:
            if isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
                chunks.setdefault(node.name, ast.get_source_segment(text, node) or "")
            elif isinstance(node, ast.ClassDef):
                for sub in node.body:
                    if isinstance(sub, ast.FunctionDef | ast.AsyncFunctionDef):
                        name = f"{node.name}.{sub.name}"
                        chunks.setdefault(name, ast.get_source_segment(text, sub) or "")
    return {k: v for k, v in chunks.items() if v.strip()}


def embed(server: Server, texts: list[str]) -> list[list[float]]:
    r = httpx.post(
        f"{server.base_url}/v1/embeddings",
        json={"input": texts},
        headers=server.headers,
        timeout=1800,
    )
    if r.status_code != 200:
        raise RuntimeError(f"HTTP {r.status_code}: {r.text[:500]}")
    data = sorted(r.json()["data"], key=lambda d: d["index"])
    return [_normalize(d["embedding"]) for d in data]


def _normalize(v: list[float]) -> list[float]:
    n = math.sqrt(sum(x * x for x in v)) or 1.0
    return [x / n for x in v]


def _dot(a: list[float], b: list[float]) -> float:
    return sum(x * y for x, y in zip(a, b, strict=True))


def token_count(server: Server, text: str) -> int:
    r = httpx.post(
        f"{server.base_url}/tokenize", json={"content": text}, headers=server.headers, timeout=60
    )
    r.raise_for_status()
    return len(r.json()["tokens"])


def _truncate(server: Server, text: str, max_tokens: int) -> str:
    n = token_count(server, text)
    while n > max_tokens:
        text = text[: int(len(text) * max_tokens / n * 0.98)]
        n = token_count(server, text)
    return text


def check_embedding(profile: Profile) -> dict:
    vram_before = vram_used_mib()
    with Server(profile, tag="check") as srv:
        result: dict = {"load_seconds": round(srv.load_seconds, 2)}
        result["vram_delta_mib"] = vram_used_mib() - vram_before
        result["ram_private_mib_idle"] = process_ram_mib(srv._proc.pid)

        limit = profile.ctx - 24
        chunks = {n: _truncate(srv, c, limit) for n, c in code_chunks().items()}
        names = list(chunks)
        tokens = sum(token_count(srv, chunks[n]) for n in names)
        start = time.monotonic()
        doc_vecs = []
        for i in range(0, len(names), 16):
            doc_vecs += embed(srv, [chunks[n] for n in names[i : i + 16]])
        index_s = time.monotonic() - start
        result["index"] = {
            "chunks": len(names),
            "tokens": tokens,
            "seconds": round(index_s, 2),
            "tokens_per_second": round(tokens / index_s, 1),
            "dims": len(doc_vecs[0]),
        }
        result["ram_private_mib_after_index"] = process_ram_mib(srv._proc.pid)

        queries = list(QUERIES)
        start = time.monotonic()
        q_vecs = embed(srv, [f"Instruct: {QUERY_TASK}\nQuery:{q}" for q in queries])
        query_ms = (time.monotonic() - start) * 1000 / len(queries)
        hits = []
        for q, qv in zip(queries, q_vecs, strict=True):
            ranked = sorted(
                range(len(names)), key=lambda i, qv=qv: _dot(qv, doc_vecs[i]), reverse=True
            )
            top = [names[i] for i in ranked[:5]]
            want = QUERIES[q]
            rank = top.index(want) + 1 if want in top else None
            hits.append({"query": q, "want": want, "rank": rank, "top5": top})
        result["retrieval"] = {
            "queries": len(queries),
            "top1": sum(h["rank"] == 1 for h in hits),
            "top5": sum(h["rank"] is not None for h in hits),
            "ms_per_query": round(query_ms, 1),
            "hits": hits,
        }

        all_text = "\n\n".join(chunks[n] for n in names)
        long_text = _truncate(srv, all_text, srv.profile.ctx - 24)
        start = time.monotonic()
        embed(srv, [long_text])
        result["max_input"] = {
            "tokens": token_count(srv, long_text),
            "seconds": round(time.monotonic() - start, 2),
        }
        try:
            embed(srv, [_truncate(srv, all_text, srv.profile.ctx * 3 // 2)])
            result["oversize_input"] = "accepted"
        except RuntimeError as e:
            result["oversize_input"] = f"rejected: {str(e)[:200]}"
        result["alive_after_oversize"] = srv.alive()
        result["log_buffers"] = [
            line.strip()
            for line in srv.log_text().splitlines()
            if "buffer size" in line and "MiB" in line
        ]
    return result


def contention(embed_profile: Profile) -> dict:
    """Generation speed of `standard` alone vs. while the CPU embedding server is indexing."""
    chat_profile = Profile.load("pq2mtp")
    chunks = [c[:2500] for c in code_chunks().values()]
    prompt = "Write a Python module that implements an LRU cache with TTL expiry and tests.\n"

    def gen_tps(srv: Server) -> float:
        r = complete_raw(srv, prompt, n_predict=512, cache_prompt=False)
        return r["timings"]["predicted_per_second"]

    out: dict = {}
    with Server(chat_profile, tag="embed-contention") as chat_srv:
        gen_tps(chat_srv)
        out["alone_tps"] = [round(gen_tps(chat_srv), 1) for _ in range(3)]
        with Server(embed_profile, tag="contention") as emb_srv:
            stop = threading.Event()
            embedded = [0]

            def loop() -> None:
                while not stop.is_set():
                    for i in range(0, len(chunks), 16):
                        if stop.is_set():
                            return
                        embedded[0] += len(embed(emb_srv, chunks[i : i + 16]))

            t = threading.Thread(target=loop, daemon=True)
            t.start()
            time.sleep(2)
            try:
                out["with_embedding_tps"] = [round(gen_tps(chat_srv), 1) for _ in range(3)]
            finally:
                stop.set()
                t.join(timeout=120)
            out["chunks_embedded_meanwhile"] = embedded[0]
    return out


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    profile = Profile.load("embedding")
    result = {"run_date": RUN_DATE, "model": profile.model, "flags": profile.flags}
    result["check"] = check_embedding(profile)
    print(json.dumps({k: v for k, v in result["check"].items() if k != "retrieval"}, indent=2))
    r = result["check"]["retrieval"]
    print(f"retrieval: top1 {r['top1']}/{r['queries']}, top5 {r['top5']}/{r['queries']}")
    result["contention_t8"] = contention(profile)
    print("t8", result["contention_t8"])
    t4 = profile.with_(flags=[f if f != "8" else "4" for f in profile.flags])
    result["contention_t4"] = contention(t4)
    print("t4", result["contention_t4"])
    (OUT_DIR / "raw").mkdir(exist_ok=True)
    (OUT_DIR / "raw" / "embedding.json").write_text(json.dumps(result, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
