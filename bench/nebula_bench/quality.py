"""B5: coding quality, scored by executable tests (Python, Rust, TypeScript)."""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from nebula_bench.client import chat
from nebula_bench.server import Profile, Server
from nebula_bench.tasks_other import RS, TS
from nebula_bench.tasks_py import PY, Task

TASKS: list[Task] = PY + RS + TS
FENCE = {"python": ("python", "py"), "rust": ("rust", "rs"), "typescript": ("typescript", "ts")}
RUSTC = Path.home() / ".cargo" / "bin" / "rustc.exe"
INSTRUCTIONS = (
    "\n\nReturn the complete implementation in a single ```{fence} code block. "
    "Do not include tests, example usage or a main function."
)


def extract_code(text: str, lang: str) -> str | None:
    names = "|".join(FENCE[lang])
    blocks = re.findall(rf"```(?:{names})[^\n]*\n(.*?)```", text, re.S | re.I)
    if not blocks:
        blocks = re.findall(r"```[^\n]*\n(.*?)```", text, re.S)
    return blocks[-1] if blocks else None


def run_tests(task: Task, code: str, timeout: int = 120) -> tuple[bool, str]:
    with tempfile.TemporaryDirectory(prefix="nebula-b5-") as d:
        tmp = Path(d)
        try:
            if task.lang == "python":
                f = tmp / "t.py"
                f.write_text(code + "\n\n# --- hidden tests ---\n" + task.test, encoding="utf-8")
                r = subprocess.run(
                    [sys.executable, str(f)],
                    capture_output=True,
                    text=True,
                    timeout=timeout,
                    cwd=tmp,
                )
            elif task.lang == "rust":
                f = tmp / "t.rs"
                f.write_text(
                    code + "\n\n#[cfg(test)]\nmod hidden_tests {\n    use super::*;\n"
                    "    #[test]\n    fn hidden() {" + task.test + "    }\n}\n",
                    encoding="utf-8",
                )
                c = subprocess.run(
                    [
                        str(RUSTC),
                        "--edition",
                        "2021",
                        "--test",
                        "-A",
                        "warnings",
                        str(f),
                        "-o",
                        str(tmp / "t.exe"),
                    ],
                    capture_output=True,
                    text=True,
                    timeout=timeout,
                    cwd=tmp,
                )
                if c.returncode != 0:
                    return False, "compile error: " + c.stderr[-600:]
                r = subprocess.run(
                    [str(tmp / "t.exe")], capture_output=True, text=True, timeout=timeout, cwd=tmp
                )
            else:
                f = tmp / "t.mts"
                f.write_text(
                    'import assert from "node:assert/strict";\n'
                    + code
                    + "\n\n// --- hidden tests ---\n"
                    + task.test,
                    encoding="utf-8",
                )
                r = subprocess.run(
                    ["node", "--experimental-strip-types", "--no-warnings", str(f)],
                    capture_output=True,
                    text=True,
                    timeout=timeout,
                    cwd=tmp,
                )
        except subprocess.TimeoutExpired:
            return False, "timeout"
        out = (r.stdout + r.stderr)[-600:]
        return r.returncode == 0, out


def selfcheck() -> list[str]:
    """Run every reference solution through its tests. Returns the ids that fail."""
    return [t.id for t in TASKS if not run_tests(t, t.reference)[0]]


def run(profile: Profile, reasoning: str, max_tokens: int, log) -> dict:
    rows = []
    with Server(profile, tag=f"b5-{reasoning}") as server:
        for task in TASKS:
            prompt = task.prompt + INSTRUCTIONS.format(fence=FENCE[task.lang][0])
            start = time.monotonic()
            row: dict = {"id": task.id, "lang": task.lang, "kind": task.kind}
            try:
                resp = chat(
                    server,
                    [{"role": "user", "content": prompt}],
                    reasoning=reasoning,
                    max_tokens=max_tokens,
                    timeout=3600,
                )
                msg = resp["choices"][0]["message"]
                text = msg.get("content") or ""
                row.update(
                    finish=resp["choices"][0].get("finish_reason"),
                    completion_tokens=resp["usage"]["completion_tokens"],
                    reasoning_chars=len(msg.get("reasoning_content") or ""),
                    tg_tps=round(resp["timings"]["predicted_per_second"], 1),
                )
                code = extract_code(text, task.lang)
                if code is None:
                    row.update(passed=False, detail="no code block")
                else:
                    ok, detail = run_tests(task, code)
                    row.update(passed=ok, detail=detail[-300:])
            except Exception as e:  # noqa: BLE001 - a failed request is a failed task
                row.update(passed=False, detail=f"error: {e}"[:300])
                if not server.alive():
                    raise
            row["seconds"] = round(time.monotonic() - start, 1)
            rows.append(row)
            log(
                f"    {task.id}: {'PASS' if row['passed'] else 'FAIL'} "
                f"({row.get('completion_tokens', '?')} tok, {row['seconds']} s)"
            )
    passed = sum(r["passed"] for r in rows)
    return {
        "reasoning": reasoning,
        "max_tokens": max_tokens,
        "passed": passed,
        "total": len(rows),
        "rows": rows,
        "by_lang": {
            lang: f"{sum(r['passed'] for r in rows if r['lang'] == lang)}/"
            f"{sum(1 for r in rows if r['lang'] == lang)}"
            for lang in ("python", "rust", "typescript")
        },
    }
