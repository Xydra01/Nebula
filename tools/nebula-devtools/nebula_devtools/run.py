"""Run a build/test/lint command inside the worktree and capture its output.

The runner spawns an ordinary child process (no detach, no new process group), so it inherits the
server's Job Object membership and is terminated when the task's job is closed (issue #30,
Requirement 6.3). A missing runner binary yields a ``RunOutput`` with ``spawn_error`` set rather
than raising (Requirement 8.1). The host owns the per-call timeout, so this imposes none.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass


@dataclass
class RunOutput:
    """The captured result of running a command.

    ``exit_code`` is ``None`` when the runner could not be spawned (``spawn_error`` is then set).
    """

    exit_code: int | None
    stdout: str
    stderr: str
    spawn_error: str | None = None

    @property
    def combined_log(self) -> str:
        """stdout followed by stderr, for the raw-log content block the host may spill to a blob."""
        if self.stdout and self.stderr:
            return f"{self.stdout}\n{self.stderr}"
        return self.stdout or self.stderr


def run_command(argv: list[str], cwd: str, env: dict[str, str] | None = None) -> RunOutput:
    """Run ``argv`` with working directory ``cwd``, capturing stdout, stderr, and the exit code.

    Spawns an ordinary child so it joins the server's Job Object (Requirement 6.3). Does not raise
    on a missing runner — returns ``RunOutput(exit_code=None, spawn_error=...)`` (Requirement 8.1).
    Imposes no timeout; the daemon's tool host enforces it and terminates the tree via the job.
    """
    try:
        completed = subprocess.run(  # noqa: S603 - argv is built from fixed runner names + args
            argv,
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
    except FileNotFoundError as e:
        return RunOutput(
            exit_code=None,
            stdout="",
            stderr="",
            spawn_error=f"runner not found: {argv[0]!r} ({e})",
        )
    except OSError as e:
        return RunOutput(
            exit_code=None,
            stdout="",
            stderr="",
            spawn_error=f"could not start {argv[0]!r}: {e}",
        )
    return RunOutput(
        exit_code=completed.returncode,
        stdout=completed.stdout or "",
        stderr=completed.stderr or "",
    )
