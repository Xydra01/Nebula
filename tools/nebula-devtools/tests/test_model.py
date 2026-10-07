"""Tests for the structured result model (issue #33, Requirements 4.4, 5.3)."""

from __future__ import annotations

from nebula_devtools.model import (
    MAX_DIAGNOSTICS,
    Diagnostic,
    Outcome,
    Severity,
    StructuredResult,
)


def _diag(file: str, line: int, column: int | None = None) -> Diagnostic:
    return Diagnostic(file=file, line=line, message="m", source="s", column=column)


def test_diagnostics_sort_stably_by_file_line_column():
    diags = [
        _diag("b.rs", 1),
        _diag("a.rs", 10),
        _diag("a.rs", 2, column=5),
        _diag("a.rs", 2, column=1),
    ]
    result = StructuredResult.build(Outcome.FAILED, "rustc", "rust", diags)
    ordered = [(d.file, d.line, d.column) for d in result.diagnostics]
    assert ordered == [("a.rs", 2, 1), ("a.rs", 2, 5), ("a.rs", 10, None), ("b.rs", 1, None)]


def test_diagnostics_are_capped_with_total_and_truncated():
    diags = [_diag("f.rs", i) for i in range(1, MAX_DIAGNOSTICS + 11)]
    result = StructuredResult.build(Outcome.FAILED, "rustc", "rust", diags)
    assert len(result.diagnostics) == MAX_DIAGNOSTICS
    assert result.total_diagnostics == MAX_DIAGNOSTICS + 10
    assert result.truncated is True


def test_under_cap_is_not_truncated():
    result = StructuredResult.build(Outcome.PASSED, "pytest", "python", [])
    assert result.truncated is False
    assert result.total_diagnostics == 0


def test_is_error_maps_outcome():
    assert StructuredResult.build(Outcome.PASSED, "s", "python").is_error is False
    assert StructuredResult.build(Outcome.FAILED, "s", "python").is_error is True
    assert StructuredResult.build(Outcome.ERRORED, "s", "python").is_error is True


def test_to_dict_omits_absent_optionals_and_column():
    d = StructuredResult.build(
        Outcome.FAILED,
        "pytest",
        "python",
        [_diag("t.py", 3)],
        tests_passed=1,
        tests_failed=1,
    ).to_dict()
    assert d["outcome"] == "failed"
    assert d["tests_passed"] == 1
    assert "note" not in d
    assert "column" not in d["diagnostics"][0]


def test_severity_values_serialize_as_strings():
    d = Diagnostic(file="f", line=1, message="m", source="s", severity=Severity.WARNING).to_dict()
    assert d["severity"] == "warning"
