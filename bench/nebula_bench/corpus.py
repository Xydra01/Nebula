"""Deterministic text corpora built from source code already on this machine.

- calibration_text(): mixed code and prose for llama-kv-mean-center (task 3.7)
- repo_dump(): a synthetic "repository dump" of real Rust and Python files, for
  long-context throughput (B1) and needle retrieval (B7)
"""

from __future__ import annotations

import functools
import random
import sysconfig
from pathlib import Path

CARGO_SRC = Path.home() / ".cargo" / "registry" / "src"
PY_STDLIB = Path(sysconfig.get_paths()["stdlib"])
DOCS = Path(__file__).resolve().parents[2] / "docs"


def _rust_files() -> list[Path]:
    files: list[Path] = []
    for index in sorted(CARGO_SRC.iterdir()):
        files.extend(sorted(index.glob("*/src/**/*.rs")))
    return files


def _python_files() -> list[Path]:
    return sorted(p for p in PY_STDLIB.glob("*.py") if p.stat().st_size < 120_000)


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


@functools.cache
def file_pool(seed: int = 7) -> list[tuple[str, str]]:
    """(display path, content) pairs, shuffled deterministically, mixing Rust and Python."""
    rng = random.Random(seed)
    pool = []
    for p in _rust_files():
        if 500 < p.stat().st_size < 40_000:
            pool.append((p.relative_to(CARGO_SRC).as_posix().split("/", 1)[1], p))
    for p in _python_files():
        if p.stat().st_size > 500:
            pool.append((f"cpython/Lib/{p.name}", p))
    rng.shuffle(pool)
    return [(name, _read(p)) for name, p in pool]


def calibration_text(target_chars: int = 400_000) -> str:
    """Roughly 50% code, 50% prose (design docs plus Python docstring-heavy modules)."""
    parts: list[str] = []
    prose = "\n\n".join(_read(p) for p in sorted(DOCS.rglob("*.md")))
    parts.append(prose[: target_chars // 2])
    size = len(parts[0])
    for name, text in file_pool(seed=11):
        if size >= target_chars:
            break
        chunk = f"\n# FILE: {name}\n{text}"
        parts.append(chunk)
        size += len(chunk)
    return "".join(parts)


def repo_dump(target_chars: int, seed: int = 7) -> list[str]:
    """File sections ('// FILE: path' + content) until target_chars is reached."""
    sections, size = [], 0
    for name, text in file_pool(seed):
        if size >= target_chars:
            break
        section = f"// FILE: {name}\n{text}\n"
        sections.append(section)
        size += len(section)
    return sections
