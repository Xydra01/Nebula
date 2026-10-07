"""The structured result model returned by the build/test/lint tools.

A tool returns a short, bounded ``StructuredResult`` (overall outcome plus ``file:line``
diagnostics) rather than a raw log. The outcome is derived from the runner's exit status, never
from the parse, so a parse miss can only make a run look worse (failed/errored), never falsely
green (issue #33, Requirement 9).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import StrEnum

#: Largest number of diagnostics inlined in a result; the rest are summarized by a count so the
#: result stays bounded (Requirement 5.3).
MAX_DIAGNOSTICS = 50


class Outcome(StrEnum):
    """The overall outcome of a run."""

    PASSED = "passed"
    FAILED = "failed"
    ERRORED = "errored"


class Severity(StrEnum):
    """The severity of a single diagnostic."""

    ERROR = "error"
    WARNING = "warning"
    NOTE = "note"


@dataclass(frozen=True)
class Diagnostic:
    """One parsed problem with its location.

    ``file`` is relative to the worktree root when the location is under it (Requirement 4.3).
    ``column`` is included only when the runner reports one (Requirement 4.1).
    """

    file: str
    line: int
    message: str
    source: str
    severity: Severity = Severity.ERROR
    column: int | None = None

    def sort_key(self) -> tuple[str, int, int]:
        """Stable ordering key: by file, then line, then column (Requirement 4.4)."""
        return (self.file, self.line, self.column if self.column is not None else -1)

    def to_dict(self) -> dict[str, object]:
        """A compact JSON-ready dict; ``column`` is omitted when absent."""
        out: dict[str, object] = {
            "file": self.file,
            "line": self.line,
            "severity": self.severity.value,
            "message": self.message,
            "source": self.source,
        }
        if self.column is not None:
            out["column"] = self.column
        return out


@dataclass
class StructuredResult:
    """The short, bounded result a tool returns.

    ``diagnostics`` is capped at :data:`MAX_DIAGNOSTICS`; ``total_diagnostics`` records the true
    count before the cap and ``truncated`` flags that the list was shortened (Requirement 5.3).
    """

    outcome: Outcome
    source: str
    language: str
    diagnostics: list[Diagnostic] = field(default_factory=list)
    total_diagnostics: int = 0
    truncated: bool = False
    tests_passed: int | None = None
    tests_failed: int | None = None
    note: str | None = None

    @classmethod
    def build(
        cls,
        outcome: Outcome,
        source: str,
        language: str,
        diagnostics: list[Diagnostic] | None = None,
        *,
        tests_passed: int | None = None,
        tests_failed: int | None = None,
        note: str | None = None,
    ) -> StructuredResult:
        """Construct a result, sorting and capping the diagnostics for a stable, bounded output."""
        diags = sorted(diagnostics or [], key=Diagnostic.sort_key)
        total = len(diags)
        truncated = total > MAX_DIAGNOSTICS
        if truncated:
            diags = diags[:MAX_DIAGNOSTICS]
        return cls(
            outcome=outcome,
            source=source,
            language=language,
            diagnostics=diags,
            total_diagnostics=total,
            truncated=truncated,
            tests_passed=tests_passed,
            tests_failed=tests_failed,
            note=note,
        )

    @property
    def is_error(self) -> bool:
        """Whether this maps to MCP ``isError`` (anything but ``passed``)."""
        return self.outcome is not Outcome.PASSED

    def to_dict(self) -> dict[str, object]:
        """A compact JSON-ready dict; ``None`` optional fields are omitted."""
        out: dict[str, object] = {
            "outcome": self.outcome.value,
            "source": self.source,
            "language": self.language,
            "diagnostics": [d.to_dict() for d in self.diagnostics],
            "total_diagnostics": self.total_diagnostics,
            "truncated": self.truncated,
        }
        if self.tests_passed is not None:
            out["tests_passed"] = self.tests_passed
        if self.tests_failed is not None:
            out["tests_failed"] = self.tests_failed
        if self.note is not None:
            out["note"] = self.note
        return out
