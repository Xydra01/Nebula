"""Parse ``pytest`` output into one diagnostic per failing test.

pytest has no built-in JSON output, so this parses the human report. Two signals are combined:

* The short-summary line ``FAILED <nodeid> - <message>`` (produced with ``-ra`` / ``--tb=short``)
  gives the failing test's node id and a short message.
* The traceback carries ``<file>:<line>: in <func>`` / ``<file>:<line>: <ErrorType>`` lines whose
  last entry for a failure is the assertion location.

For each failing test we emit a :class:`~nebula_devtools.model.Diagnostic` at the best location we
can recover (the node id's file:line, else the last traceback location), with the summary message.
"""

from __future__ import annotations

import re
from pathlib import PurePath

from ..model import Diagnostic, Severity

# "FAILED tests/test_x.py::test_name - AssertionError: expected 1 got 2"
_FAILED_SUMMARY = re.compile(r"^FAILED\s+(?P<nodeid>\S+?)(?:\s+-\s+(?P<message>.*))?$")
# A node id is "<file>::<test>" (optionally with "::Class::method"); the file is the first segment.
_NODEID = re.compile(r"^(?P<file>[^:]+(?:\.py))::(?P<rest>.+)$")
# A traceback location line: "tests/test_x.py:12: in test_name" or "tests/test_x.py:12: Assertion".
_TB_LOCATION = re.compile(r"^(?P<file>.+\.py):(?P<line>\d+): ")


def _relative(file: str, worktree_root: str) -> str:
    try:
        return str(PurePath(file).relative_to(PurePath(worktree_root)))
    except ValueError:
        return file


def _norm(file: str) -> str:
    """Normalize a path for separator-insensitive keying (pytest mixes ``\\`` and ``/``)."""
    return file.replace("\\", "/")


def parse_pytest(output: str, worktree_root: str) -> list[Diagnostic]:
    """Parse pytest output into one diagnostic per failing test.

    Returns an empty list when no ``FAILED`` summary lines are present (for example an all-pass run,
    or a collection error whose outcome the caller derives from the exit status instead).
    """
    lines = output.splitlines()

    # Collect the last traceback location seen per test file, as a fallback line number.
    # Map file -> most recent (line, 1-based) seen in a traceback.
    last_location: dict[str, int] = {}
    for line in lines:
        m = _TB_LOCATION.match(line.strip())
        if m:
            last_location[_norm(m.group("file"))] = int(m.group("line"))

    diagnostics: list[Diagnostic] = []
    for line in lines:
        summary = _FAILED_SUMMARY.match(line.strip())
        if not summary:
            continue
        nodeid = summary.group("nodeid")
        message = (summary.group("message") or "test failed").strip()
        node = _NODEID.match(nodeid)
        file = node.group("file") if node else nodeid
        # Prefer a traceback location for this file; default to line 1 if none was captured.
        # Key on the separator-normalized path so a "/" nodeid matches a "\\" traceback line.
        line_no = last_location.get(_norm(file), 1)
        diagnostics.append(
            Diagnostic(
                file=_relative(file, worktree_root),
                line=line_no,
                message=f"{nodeid}: {message}",
                source="pytest",
                severity=Severity.ERROR,
            )
        )
    return diagnostics
