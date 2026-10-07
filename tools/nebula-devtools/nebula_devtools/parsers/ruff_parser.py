"""Parse ``ruff check --output-format json`` output into diagnostics.

Ruff emits a JSON array of findings, each with ``filename``, a ``location`` (``row``/``column``),
a ``code``, and a ``message``.
"""

from __future__ import annotations

import json
from pathlib import PurePath

from ..model import Diagnostic, Severity


def _relative(file: str, worktree_root: str) -> str:
    try:
        return str(PurePath(file).relative_to(PurePath(worktree_root)))
    except ValueError:
        return file


def parse_ruff_json(output: str, worktree_root: str) -> list[Diagnostic]:
    """Parse ruff's JSON findings into diagnostics (source ``ruff``, severity warning).

    A non-JSON or unexpected payload yields an empty list rather than raising; the caller's
    exit-status check still reports the run as failed when ruff found problems.
    """
    text = output.strip()
    if not text:
        return []
    try:
        findings = json.loads(text)
    except json.JSONDecodeError:
        return []
    if not isinstance(findings, list):
        return []
    diagnostics: list[Diagnostic] = []
    for finding in findings:
        if not isinstance(finding, dict):
            continue
        file = finding.get("filename")
        location = finding.get("location")
        if not isinstance(file, str) or not isinstance(location, dict):
            continue
        row = location.get("row")
        if not isinstance(row, int):
            continue
        column = location.get("column")
        code = finding.get("code")
        message = finding.get("message", "")
        if isinstance(code, str) and code:
            message = f"{code}: {message}"
        diagnostics.append(
            Diagnostic(
                file=_relative(file, worktree_root),
                line=row,
                column=column if isinstance(column, int) else None,
                message=message,
                source="ruff",
                severity=Severity.WARNING,
            )
        )
    return diagnostics
