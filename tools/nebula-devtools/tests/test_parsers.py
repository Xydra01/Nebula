"""Fixture-driven parser tests (issue #33, Requirement 4).

Each test reads a captured runner output from ``fixtures/`` and asserts the exact diagnostics the
parser produces. These need no external toolchain — they read fixtures — so they run anywhere.
"""

from __future__ import annotations

from pathlib import Path

from nebula_devtools.model import Severity
from nebula_devtools.parsers.cargo_parser import parse_cargo_json
from nebula_devtools.parsers.nextest_parser import parse_nextest
from nebula_devtools.parsers.pytest_parser import parse_pytest
from nebula_devtools.parsers.ruff_parser import parse_ruff_json

FIXTURES = Path(__file__).parent / "fixtures"


def _read(name: str) -> str:
    return (FIXTURES / name).read_text(encoding="utf-8")


def test_pytest_one_failure_has_location():
    diags = parse_pytest(_read("pytest_one_failure.txt"), worktree_root="C:/work/sample")
    assert len(diags) == 1
    d = diags[0]
    # Path is reported relative to the worktree root.
    assert d.file.replace("\\", "/") == "tests/test_math.py"
    assert d.line == 12
    assert d.source == "pytest"
    assert "test_adds_wrong" in d.message


def test_nextest_one_failure_has_panic_location():
    diags = parse_nextest(_read("nextest_one_failure.txt"), worktree_root="C:/work/sample")
    assert len(diags) == 1
    d = diags[0]
    assert d.file.replace("\\", "/") == "src/lib.rs"
    assert d.line == 18
    assert d.column == 9
    assert d.source == "nextest"
    assert "adds_wrong" in d.message


def test_cargo_build_error_and_warning():
    diags = parse_cargo_json(_read("cargo_build_error.jsonl"), worktree_root="", source="rustc")
    # One error and one warning, sorted by (file, line).
    assert len(diags) == 2
    by_line = {d.line: d for d in diags}
    assert by_line[7].severity is Severity.ERROR
    assert "cannot find value" in by_line[7].message
    assert "E0425" in by_line[7].message
    assert by_line[7].column == 5
    assert by_line[3].severity is Severity.WARNING
    assert all(d.source == "rustc" for d in diags)


def test_clippy_finding():
    diags = parse_cargo_json(_read("clippy_finding.jsonl"), worktree_root="", source="clippy")
    assert len(diags) == 1
    d = diags[0]
    assert d.file.replace("\\", "/") == "src/main.rs"
    assert d.line == 10
    assert d.severity is Severity.WARNING
    assert "collapsible_if" in d.message
    assert d.source == "clippy"


def test_ruff_findings():
    diags = parse_ruff_json(_read("ruff_findings.json"), worktree_root="")
    assert len(diags) == 2
    assert [d.line for d in diags] == [1, 5]
    assert diags[0].file.replace("\\", "/") == "sample/app.py"
    assert diags[0].column == 8
    assert "F401" in diags[0].message
    assert all(d.source == "ruff" for d in diags)
    assert all(d.severity is Severity.WARNING for d in diags)


def test_malformed_cargo_json_is_skipped_not_crashed():
    # A non-JSON line and a JSON line without spans both skip cleanly; no false diagnostics.
    no_span = '{"reason":"compiler-message","message":{"level":"error","message":"x","spans":[]}}'
    output = f"not json\n{no_span}\n"
    assert parse_cargo_json(output, worktree_root="", source="rustc") == []


def test_empty_ruff_output_is_no_findings():
    assert parse_ruff_json("", worktree_root="") == []
    assert parse_ruff_json("[]", worktree_root="") == []
