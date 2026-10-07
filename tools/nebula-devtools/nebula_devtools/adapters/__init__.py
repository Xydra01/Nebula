"""Per-language adapters that run build/test/lint runners and return a structured result.

Each adapter exposes ``build``, ``test``, and ``lint`` returning ``(StructuredResult, RunOutput)``.
The ``RunOutput`` carries the raw log for the second MCP content block; the ``StructuredResult`` is
the short, bounded result. Outcome is derived from the runner's exit status, never from the parse
(issue #33, Requirement 9), so a parse miss can only make a run look failed/errored, never passed.
"""

from __future__ import annotations

from ..model import Outcome, StructuredResult
from ..run import RunOutput


def outcome_from_exit(run: RunOutput, parsed_failures: int) -> Outcome:
    """Derive the overall outcome from the runner's exit status (exit-status-first, Requirement 9).

    * The runner could not be spawned -> ``errored``.
    * exit 0 -> ``passed``.
    * non-zero exit with at least one parsed diagnostic -> ``failed``.
    * non-zero exit with nothing parsed -> ``errored`` (ran, but output unparseable) — never passed.
    """
    if run.exit_code is None:
        return Outcome.ERRORED
    if run.exit_code == 0:
        return Outcome.PASSED
    return Outcome.FAILED if parsed_failures > 0 else Outcome.ERRORED


def errored_result(source: str, language: str, note: str) -> StructuredResult:
    """A result for a run that could not be performed (missing runner, bad request)."""
    return StructuredResult.build(Outcome.ERRORED, source, language, [], note=note)
