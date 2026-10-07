"""The Rust adapter: ``cargo build`` (build), ``cargo nextest`` (test), ``cargo clippy`` (lint).

Build and clippy use ``--message-format json`` so diagnostics carry exact ``file:line:column``;
nextest's pass/fail comes from its exit status, with panic locations recovered from its output.
"""

from __future__ import annotations

from ..model import Outcome, StructuredResult
from ..parsers.cargo_parser import parse_cargo_json
from ..parsers.nextest_parser import parse_nextest
from ..run import RunOutput, run_command
from . import errored_result, outcome_from_exit

LANGUAGE = "rust"


class RustAdapter:
    """Runs Rust build/test/lint runners in the worktree and returns structured results."""

    language = LANGUAGE

    def build(self, cwd: str) -> tuple[StructuredResult, RunOutput]:
        """Run ``cargo build`` with JSON messages so compiler errors carry locations."""
        run = run_command(
            ["cargo", "build", "--message-format", "json"],
            cwd=cwd,
        )
        if run.exit_code is None:
            return errored_result("rustc", LANGUAGE, run.spawn_error or "cargo unavailable"), run
        diags = parse_cargo_json(run.stdout, worktree_root=cwd, source="rustc")
        errors = [d for d in diags if d.severity.value == "error"]
        outcome = outcome_from_exit(run, len(errors))
        note = None
        if outcome is Outcome.ERRORED and not errors:
            note = "cargo build exited non-zero but no compiler errors were parsed"
        return StructuredResult.build(outcome, "rustc", LANGUAGE, diags, note=note), run

    def test(self, cwd: str) -> tuple[StructuredResult, RunOutput]:
        """Run ``cargo nextest run``; pass/fail is the exit status, locations from panic lines."""
        run = run_command(
            ["cargo", "nextest", "run"],
            cwd=cwd,
        )
        if run.exit_code is None:
            return errored_result(
                "nextest", LANGUAGE, run.spawn_error or "nextest unavailable"
            ), run
        failures = parse_nextest(run.combined_log, worktree_root=cwd)
        outcome = outcome_from_exit(run, len(failures))
        note = None
        if outcome is Outcome.ERRORED and not failures:
            note = "cargo nextest exited non-zero but no failing tests were parsed"
        result = StructuredResult.build(
            outcome,
            "nextest",
            LANGUAGE,
            failures,
            tests_failed=len(failures) if outcome is not Outcome.PASSED else 0,
            note=note,
        )
        return result, run

    def lint(self, cwd: str) -> tuple[StructuredResult, RunOutput]:
        """Run ``cargo clippy`` with JSON messages."""
        run = run_command(
            ["cargo", "clippy", "--message-format", "json"],
            cwd=cwd,
        )
        if run.exit_code is None:
            return errored_result("clippy", LANGUAGE, run.spawn_error or "clippy unavailable"), run
        diags = parse_cargo_json(run.stdout, worktree_root=cwd, source="clippy")
        errors = [d for d in diags if d.severity.value == "error"]
        outcome = outcome_from_exit(run, len(errors))
        note = None
        if outcome is Outcome.ERRORED and not errors:
            note = "cargo clippy exited non-zero but no errors were parsed"
        return StructuredResult.build(outcome, "clippy", LANGUAGE, diags, note=note), run
