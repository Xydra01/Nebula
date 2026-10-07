"""Parse ``cargo build``/``cargo clippy`` ``--message-format json`` output into diagnostics.

Both commands emit one JSON object per line; a ``compiler-message`` carries a ``message`` with a
``level`` and ``spans``, where the primary span gives the file, line, and column. This one parser
serves both ``build.run`` (rustc errors/warnings) and ``lint.run`` (clippy findings); the ``source``
is set by the caller.
"""

from __future__ import annotations

import json
from pathlib import PurePath

from ..model import Diagnostic, Severity

_LEVEL_TO_SEVERITY = {
    "error": Severity.ERROR,
    "warning": Severity.WARNING,
    "note": Severity.NOTE,
    "help": Severity.NOTE,
}


def _relative(file: str, worktree_root: str) -> str:
    """Make a cargo-reported path relative to the worktree root when possible."""
    try:
        return str(PurePath(file).relative_to(PurePath(worktree_root)))
    except ValueError:
        return file


def parse_cargo_json(output: str, worktree_root: str, source: str) -> list[Diagnostic]:
    """Parse cargo JSON lines into diagnostics.

    ``source`` is ``"rustc"`` for ``build.run`` or ``"clippy"`` for ``lint.run``. Lines that are not
    JSON, or not ``compiler-message`` records, or carry no primary span, are skipped — a malformed
    stream yields fewer diagnostics but never a crash (the caller's exit-status check keeps the
    outcome honest).
    """
    diagnostics: list[Diagnostic] = []
    for line in output.splitlines():
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("reason") != "compiler-message":
            continue
        message = record.get("message")
        if not isinstance(message, dict):
            continue
        level = message.get("level", "error")
        severity = _LEVEL_TO_SEVERITY.get(level, Severity.ERROR)
        # Notes/help without their own location are context for a primary diagnostic; skip them.
        if severity is Severity.NOTE:
            continue
        text = message.get("message", "")
        code = message.get("code")
        if isinstance(code, dict) and code.get("code"):
            text = f"{text} [{code['code']}]"
        spans = message.get("spans")
        if not isinstance(spans, list):
            continue
        primary = next(
            (s for s in spans if isinstance(s, dict) and s.get("is_primary")),
            None,
        )
        if primary is None:
            continue
        file = primary.get("file_name")
        line_no = primary.get("line_start")
        if not isinstance(file, str) or not isinstance(line_no, int):
            continue
        column = primary.get("column_start")
        diagnostics.append(
            Diagnostic(
                file=_relative(file, worktree_root),
                line=line_no,
                column=column if isinstance(column, int) else None,
                message=text,
                source=source,
                severity=severity,
            )
        )
    return diagnostics
