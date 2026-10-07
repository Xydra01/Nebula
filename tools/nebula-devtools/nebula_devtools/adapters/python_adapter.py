"""The Python adapter: tests via ``uv run pytest``, linting via ``uv run ruff`` (issue #33).

Python has no separate build step, so ``build`` is a no-op ``passed`` with a note, keeping a
mixed-language caller from being blocked. ``pyright`` is deferred (it needs Node).
"""

from __future__ import annotations

from ..model import Outcome, StructuredResult
from ..parsers.pytest_parser import parse_pytest
from ..parsers.ruff_parser import parse_ruff_json
from ..run import RunOutput, run_command
from . import errored_result, outcome_from_exit

LANGUAGE = "python"


class PythonAdapter:
    """Runs Python test/lint runners in the worktree and returns structured results."""

    language = LANGUAGE

    def test(self, cwd: str) -> tuple[StructuredResult, RunOutput]:
        """Run ``uv run pytest`` with short tracebacks and a failure summary for locations."""
        run = run_command(
            ["uv", "run", "pytest", "-q", "--tb=short", "-ra"],
            cwd=cwd,
        )
        if run.exit_code is None:
            return errored_result("pytest", LANGUAGE, run.spawn_error or "pytest unavailable"), run
        failures = parse_pytest(run.combined_log, worktree_root=cwd)
        outcome = outcome_from_exit(run, len(failures))
        note = None
        if outcome is Outcome.ERRORED and not failures:
            note = "pytest exited non-zero but no failing tests were parsed"
        result = StructuredResult.build(
            outcome,
            "pytest",
            LANGUAGE,
            failures,
            tests_failed=len(failures) if outcome is not Outcome.PASSED else 0,
            note=note,
        )
        return result, run

    def lint(self, cwd: str) -> tuple[StructuredResult, RunOutput]:
        """Run ``uv run ruff check`` with JSON output."""
        run = run_command(
            ["uv", "run", "ruff", "check", "--output-format", "json", "."],
            cwd=cwd,
        )
        if run.exit_code is None:
            return errored_result("ruff", LANGUAGE, run.spawn_error or "ruff unavailable"), run
        findings = parse_ruff_json(run.stdout, worktree_root=cwd)
        outcome = outcome_from_exit(run, len(findings))
        note = None
        if outcome is Outcome.ERRORED and not findings:
            note = "ruff exited non-zero but no findings were parsed"
        return StructuredResult.build(outcome, "ruff", LANGUAGE, findings, note=note), run

    def build(self, cwd: str) -> tuple[StructuredResult, RunOutput]:
        """Python has no separate build step; report a no-op pass with a note."""
        run = RunOutput(exit_code=0, stdout="", stderr="")
        result = StructuredResult.build(
            Outcome.PASSED,
            "python",
            LANGUAGE,
            [],
            note="python has no separate build step",
        )
        return result, run
