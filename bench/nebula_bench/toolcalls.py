"""B6: tool-call reliability on 100 generated cases.

Modes:
- native:  OpenAI `tools` parameter (llama-server applies its tool-call grammar)
- prompted: tools described in the system prompt, JSON answer in plain text, no constraint
- schema:  same prompt, but the answer is forced to a JSON schema (grammar-constrained)
"""

from __future__ import annotations

import json
import random
import re
from dataclasses import dataclass, field

from nebula_bench.client import chat
from nebula_bench.server import Profile, Server


def _fn(name: str, desc: str, props: dict, required: list[str]) -> dict:
    return {
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {"type": "object", "properties": props, "required": required},
        },
    }


S, I, B = {"type": "string"}, {"type": "integer"}, {"type": "boolean"}
TOOLS = [
    _fn(
        "read_file",
        "Read a text file. Optional 1-based inclusive line range.",
        {"path": S, "start_line": I, "end_line": I},
        ["path"],
    ),
    _fn(
        "write_file",
        "Create or overwrite a file with the given content.",
        {"path": S, "content": S},
        ["path", "content"],
    ),
    _fn("list_dir", "List the entries of a directory.", {"path": S, "recursive": B}, ["path"]),
    _fn(
        "search_code",
        "Search files for a regular expression.",
        {"pattern": S, "path": S, "case_sensitive": B},
        ["pattern"],
    ),
    _fn(
        "run_command",
        "Run a shell command in the workspace.",
        {"command": S, "cwd": S, "timeout_s": I},
        ["command"],
    ),
    _fn(
        "git_commit",
        "Commit the given files with a message.",
        {"message": S, "files": {"type": "array", "items": S}},
        ["message", "files"],
    ),
    _fn("web_search", "Search the web.", {"query": S, "max_results": I}, ["query"]),
    _fn("http_get", "Fetch a URL and return the body as text.", {"url": S}, ["url"]),
]
TOOL_NAMES = [t["function"]["name"] for t in TOOLS]


@dataclass
class Case:
    request: str
    # Each expected call: (tool name, {arg: exact value or ("contains", text)}).
    expected: list[tuple[str, dict]] = field(default_factory=list)


def _contains(text: str) -> tuple[str, str]:
    return ("contains", text)


def make_cases(seed: int = 5) -> list[Case]:
    rng = random.Random(seed)
    files = [
        "src/main.rs",
        "crates/proto/src/lib.rs",
        "bench/nebula_bench/perf.py",
        "README.md",
        "docs/adr/README.md",
        "app/api/routes.py",
        "web/src/App.tsx",
        "Cargo.toml",
        "scripts/install-hooks.ps1",
        "tests/test_parser.py",
    ]
    dirs = ["src", "crates/daemon", "bench/profiles", "docs", "web/src/components"]
    patterns = ["TODO", "fn main", "unwrap\\(\\)", "class .*Error", "import requests"]
    cases: list[Case] = []
    for f in rng.sample(files, 8):
        cases.append(
            Case(
                rng.choice(
                    [
                        "Show me {f}.",
                        "Open {f} so I can see it.",
                        "What's in {f}?",
                        "Read the file {f}.",
                    ]
                ).format(f=f),
                [("read_file", {"path": f})],
            )
        )
    for f in rng.sample(files, 4):
        a = rng.randint(5, 40)
        cases.append(
            Case(
                f"Read lines {a} to {a + 20} of {f}.",
                [("read_file", {"path": f, "start_line": a, "end_line": a + 20})],
            )
        )
    for d in dirs:
        cases.append(
            Case(
                rng.choice(["List the files in {d}.", "What's inside the {d} folder?"]).format(d=d),
                [("list_dir", {"path": d})],
            )
        )
    for d in dirs[:3]:
        cases.append(
            Case(
                f"Recursively list everything under {d}.",
                [("list_dir", {"path": d, "recursive": True})],
            )
        )
    for p in patterns:
        cases.append(
            Case(f"Search the codebase for the regex `{p}`.", [("search_code", {"pattern": p})])
        )
    for p, d in zip(patterns[:4], dirs[:4], strict=True):
        cases.append(
            Case(
                f"Find `{p}` (case-sensitive) but only inside {d}.",
                [("search_code", {"pattern": p, "path": d, "case_sensitive": True})],
            )
        )
    commands = [
        ("cargo test", "crates/daemon"),
        ("uv run pytest", "bench"),
        ("npm run build", "web"),
        ("cargo clippy --workspace", None),
        ("git status", None),
        ("cargo fmt --all --check", None),
    ]
    for cmd, cwd in commands:
        if cwd:
            cases.append(
                Case(
                    f"Run `{cmd}` in the {cwd} directory.",
                    [("run_command", {"command": _contains(cmd), "cwd": cwd})],
                )
            )
        else:
            cases.append(Case(f"Run `{cmd}`.", [("run_command", {"command": _contains(cmd)})]))
    cases.append(
        Case(
            "Run `cargo build --release` with a timeout of 600 seconds.",
            [("run_command", {"command": _contains("cargo build --release"), "timeout_s": 600})],
        )
    )
    for f, text in [
        ("notes/todo.md", "- write ADR-004"),
        ("hello.py", 'print("hello")'),
        (".env.example", "PORT=8080"),
        ("docs/CHANGELOG.md", "## 0.1.0"),
    ]:
        cases.append(
            Case(
                f"Create the file {f} containing exactly: {text}",
                [("write_file", {"path": f, "content": _contains(text)})],
            )
        )
    for msg, fs in [
        ("fix: handle empty input", ["src/parse.rs"]),
        ("docs: add ADR-004", ["docs/adr/ADR-004.md", "docs/adr/README.md"]),
        ("chore: bump deps", ["Cargo.toml", "Cargo.lock"]),
    ]:
        cases.append(
            Case(
                f'Commit {" and ".join(fs)} with the message "{msg}".',
                [("git_commit", {"message": msg, "files": fs})],
            )
        )
    for q in [
        "llama.cpp ctx-checkpoints flag",
        "tokio named pipe server example",
        "ratatui table widget scrolling",
        "sqlite-vec rust bindings",
    ]:
        cases.append(
            Case(f"Search the web for: {q}", [("web_search", {"query": _contains(q.split()[0])})])
        )
    cases.append(
        Case(
            "Search the web for 'rmcp crate' and give me 3 results.",
            [("web_search", {"query": _contains("rmcp"), "max_results": 3})],
        )
    )
    for u in [
        "https://docs.rs/tokio/latest/tokio/",
        "https://github.com/PrismML-Eng/llama.cpp",
        "https://example.com/api/status",
    ]:
        cases.append(Case(f"Fetch {u}", [("http_get", {"url": u})]))
    for a, b in [
        ("Cargo.toml", "README.md"),
        ("src/main.rs", "src/lib.rs"),
        ("bench/pyproject.toml", "bench/profiles/standard.toml"),
    ]:
        cases.append(
            Case(
                f"Read both {a} and {b}.", [("read_file", {"path": a}), ("read_file", {"path": b})]
            )
        )
    cases.append(
        Case(
            "List the src directory and also search it for TODO.",
            [("list_dir", {"path": "src"}), ("search_code", {"pattern": "TODO", "path": "src"})],
        )
    )
    for q in [
        "What does HTTP status 404 mean?",
        "Explain the difference between a mutex and a semaphore in two sentences.",
        "What is 12 squared?",
        "Say hello.",
        "Which is larger, a kilobyte or a megabyte?",
    ]:
        cases.append(Case(q, []))
    # Pad to 100 with more single-call variety from the same generators.
    while len(cases) < 100:
        f = rng.choice(files)
        kind = rng.randrange(3)
        if kind == 0:
            cases.append(Case(f"Please read {f}.", [("read_file", {"path": f})]))
        elif kind == 1:
            p = rng.choice(patterns)
            cases.append(
                Case(f"Look for `{p}` across the repo.", [("search_code", {"pattern": p})])
            )
        else:
            d = rng.choice(dirs)
            cases.append(Case(f"Show the directory listing for {d}.", [("list_dir", {"path": d})]))
    return cases[:100]


SYSTEM = (
    "You are a coding agent working in a repository. Use the tools when the request needs "
    "them; answer directly when no tool is needed. Use repository-relative paths exactly "
    "as the user writes them."
)
PROMPTED = (
    SYSTEM
    + "\n\nAvailable tools (JSON schema):\n"
    + json.dumps(TOOLS, indent=1)
    + '\n\nRespond with JSON only, in the form {"calls": [{"tool": "<name>", "arguments": '
    "{...}}]}. Use an empty list when no tool is needed."
)
CALLS_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["calls"],
    "properties": {
        "calls": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["tool", "arguments"],
                "properties": {"tool": {"enum": TOOL_NAMES}, "arguments": {"type": "object"}},
            },
        }
    },
}


# Text each expected search pattern is meant to find; any regex that matches it is accepted.
PATTERN_SAMPLES = {
    "TODO": "// TODO: fix",
    "fn main": "fn main() {",
    "unwrap\\(\\)": "let x = y.unwrap();",
    "class .*Error": "class ParseError(Exception):",
    "import requests": "import requests",
}


def _arg_ok(actual, expected, key: str = "") -> bool:
    if isinstance(expected, tuple) and expected[0] == "contains":
        return isinstance(actual, str) and expected[1].lower() in actual.lower()
    if isinstance(expected, str) and isinstance(actual, str):
        if key == "pattern" and expected in PATTERN_SAMPLES and actual != expected:
            try:
                return re.search(actual, PATTERN_SAMPLES[expected]) is not None
            except re.error:
                return False
        if key == "path":
            return actual.strip().strip("/").replace("\\", "/") == expected.strip("/")
        return actual.strip() == expected
    return actual == expected


def score(case: Case, calls: list[tuple[str, dict]] | None) -> dict:
    if calls is None:
        return {"valid": False, "tool_ok": False, "args_ok": False}
    want = sorted(case.expected, key=lambda c: c[0])
    got = sorted(calls, key=lambda c: c[0])
    tool_ok = [c[0] for c in got] == [c[0] for c in want]
    args_ok = tool_ok and all(
        all(_arg_ok(g[1].get(k), v, k) for k, v in w[1].items())
        for g, w in zip(got, want, strict=True)
    )
    return {"valid": True, "tool_ok": tool_ok, "args_ok": args_ok}


def rescore(result: dict) -> dict:
    """Re-apply the current scorer to stored failures; earlier passes still pass."""
    cases = make_cases()
    for mode in result["modes"].values():
        n = result["cases"]
        fails = []
        for row in mode["failures"]:
            calls = None if row["got"] is None else [(c[0], c[1]) for c in row["got"]]
            row.update(score(cases[row["i"]], calls))
            if not row["args_ok"]:
                fails.append(row)
        old = mode["summary"]
        for k in ("valid", "tool_ok", "args_ok"):
            failed = sum(not r[k] for r in mode["failures"])
            mode.setdefault("summary_strict", dict(old))
            old[k] = round(100 * (n - failed) / n, 1)
        mode["failures"] = fails
    return result


def _parse_calls_json(text: str) -> list[tuple[str, dict]] | None:
    text = text.strip()
    if text.startswith("```"):
        text = text.strip("`").split("\n", 1)[-1]
    try:
        data = json.loads(text[text.find("{") : text.rfind("}") + 1])
        return [
            (c["tool"], c.get("arguments") or {k: v for k, v in c.items() if k != "tool"})
            for c in data["calls"]
        ]
    except (ValueError, KeyError, TypeError):
        return None


def ask(server: Server, case: Case, mode: str) -> tuple[list | None, str]:
    if mode == "native":
        resp = chat(
            server,
            [{"role": "system", "content": SYSTEM}, {"role": "user", "content": case.request}],
            reasoning="none",
            max_tokens=768,
            tools=TOOLS,
            tool_choice="auto",
        )
        msg = resp["choices"][0]["message"]
        calls = []
        for c in msg.get("tool_calls") or []:
            try:
                calls.append(
                    (c["function"]["name"], json.loads(c["function"]["arguments"] or "{}"))
                )
            except (json.JSONDecodeError, TypeError):
                return None, str(c)[:200]
        return calls, (msg.get("content") or "")[:120]
    extra = {}
    if mode == "schema":
        extra["response_format"] = {
            "type": "json_schema",
            "json_schema": {"name": "calls", "schema": CALLS_SCHEMA, "strict": True},
        }
    resp = chat(
        server,
        [{"role": "system", "content": PROMPTED}, {"role": "user", "content": case.request}],
        reasoning="none",
        max_tokens=768,
        **extra,
    )
    text = resp["choices"][0]["message"].get("content") or ""
    return _parse_calls_json(text), text[:200]


def run(profile: Profile, modes: list[str], log) -> dict:
    cases = make_cases()
    out: dict = {"cases": len(cases), "modes": {}}
    with Server(profile, tag="b6") as server:
        for mode in modes:
            rows = []
            for i, case in enumerate(cases):
                try:
                    calls, raw = ask(server, case, mode)
                except Exception as e:  # noqa: BLE001 - a failed request is a failed case
                    calls, raw = None, f"error: {e}"[:200]
                    if not server.alive():
                        raise
                s = score(case, calls)
                rows.append(
                    {
                        "i": i,
                        "request": case.request,
                        **s,
                        "got": calls,
                        "raw": raw if not s["args_ok"] else "",
                    }
                )
            n = len(rows)
            summary = {
                k: round(100 * sum(r[k] for r in rows) / n, 1)
                for k in ("valid", "tool_ok", "args_ok")
            }
            out["modes"][mode] = {
                "summary": summary,
                "failures": [r for r in rows if not r["args_ok"]],
            }
            log(
                f"    {mode}: valid {summary['valid']}%, tool {summary['tool_ok']}%, "
                f"args {summary['args_ok']}%"
            )
    return out
