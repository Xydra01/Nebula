"""End-to-end adapter tests against real toolchains (issue #33, Requirements 2.4, 2.5, 8.1).

The headline acceptance for #33: a project with one failing ``pytest`` test and one with a failing
``cargo nextest`` test each return overall ``failed`` with exactly that one failure and its
``file:line``. These tests build a tiny throwaway project in a temp dir and run the adapter end to
end, so they need the real toolchain:

* the Python test needs ``uv`` on PATH;
* the Rust test needs ``cargo`` with the ``nextest`` subcommand.

Each skips cleanly when its runner is absent (Requirement 8.1 is still proven by the
``missing_runner`` tests below, which need no toolchain at all). They are marked ``e2e`` so a run
can select or exclude them (``pytest -m e2e`` / ``pytest -m "not e2e"``).
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from nebula_devtools.adapters.python_adapter import PythonAdapter
from nebula_devtools.adapters.rust_adapter import RustAdapter
from nebula_devtools.model import Outcome

pytestmark = pytest.mark.e2e


def _have(cmd: str) -> bool:
    return shutil.which(cmd) is not None


def _have_nextest() -> bool:
    if not _have("cargo"):
        return False
    try:
        proc = subprocess.run(  # noqa: S603 - fixed argv, no shell
            ["cargo", "nextest", "--version"],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return False
    return proc.returncode == 0


# --------------------------------------------------------------------------------------------------
# Missing-runner path (Requirement 8.1) — no toolchain required, so these always run.
# --------------------------------------------------------------------------------------------------


def test_python_test_missing_runner_is_errored(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    # Force the uv runner to look absent by emptying PATH so the spawn fails cleanly.
    monkeypatch.setenv("PATH", "")
    monkeypatch.setattr(shutil, "which", lambda _cmd, path=None: None)
    result, run = PythonAdapter().test(str(tmp_path))
    assert result.outcome is Outcome.ERRORED
    assert result.is_error is True
    assert run.exit_code is None
    assert run.spawn_error is not None


def test_rust_test_missing_runner_is_errored(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("PATH", "")
    monkeypatch.setattr(shutil, "which", lambda _cmd, path=None: None)
    result, run = RustAdapter().test(str(tmp_path))
    assert result.outcome is Outcome.ERRORED
    assert result.is_error is True
    assert run.exit_code is None
    assert run.spawn_error is not None


# --------------------------------------------------------------------------------------------------
# Python: one failing pytest test -> failed with exactly that failure and its file:line.
# --------------------------------------------------------------------------------------------------


@pytest.mark.skipif(not _have("uv"), reason="uv is not installed")
def test_python_one_failing_test_reports_that_failure(tmp_path: Path):
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "sample"\nversion = "0.0.0"\nrequires-python = ">=3.12"\n',
        encoding="utf-8",
    )
    tests_dir = tmp_path / "tests"
    tests_dir.mkdir()
    # Line 1 blank, def on line 2, the failing assert on line 3.
    (tests_dir / "test_math.py").write_text(
        "\ndef test_adds_wrong():\n    assert 1 + 1 == 3\n",
        encoding="utf-8",
    )

    result, run = PythonAdapter().test(str(tmp_path))

    assert result.outcome is Outcome.FAILED, run.combined_log
    assert result.is_error is True
    assert len(result.diagnostics) == 1, [d.to_dict() for d in result.diagnostics]
    d = result.diagnostics[0]
    assert d.file.replace("\\", "/") == "tests/test_math.py"
    assert d.line == 3
    assert d.source == "pytest"
    assert "test_adds_wrong" in d.message


@pytest.mark.skipif(not _have("uv"), reason="uv is not installed")
def test_python_passing_suite_is_passed(tmp_path: Path):
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "sample"\nversion = "0.0.0"\nrequires-python = ">=3.12"\n',
        encoding="utf-8",
    )
    tests_dir = tmp_path / "tests"
    tests_dir.mkdir()
    (tests_dir / "test_ok.py").write_text(
        "def test_adds():\n    assert 1 + 1 == 2\n",
        encoding="utf-8",
    )

    result, run = PythonAdapter().test(str(tmp_path))

    assert result.outcome is Outcome.PASSED, run.combined_log
    assert result.is_error is False
    assert result.diagnostics == []


# --------------------------------------------------------------------------------------------------
# Rust: one failing nextest test -> failed with exactly that failure and its file:line.
# --------------------------------------------------------------------------------------------------


def _write_cargo_project(root: Path, lib_body: str) -> None:
    (root / "Cargo.toml").write_text(
        '[package]\nname = "sample"\nversion = "0.0.0"\nedition = "2021"\n\n[dependencies]\n',
        encoding="utf-8",
    )
    src = root / "src"
    src.mkdir()
    (src / "lib.rs").write_text(lib_body, encoding="utf-8")


@pytest.mark.skipif(not _have_nextest(), reason="cargo nextest is not installed")
def test_rust_one_failing_test_reports_that_failure(tmp_path: Path):
    # The failing assert sits on a known line so we can assert file:line exactly.
    lib = (
        "pub fn add(a: i32, b: i32) -> i32 {\n"
        "    a + b\n"
        "}\n"
        "\n"
        "#[cfg(test)]\n"
        "mod tests {\n"
        "    use super::*;\n"
        "\n"
        "    #[test]\n"
        "    fn adds_wrong() {\n"
        "        assert_eq!(add(1, 1), 3);\n"  # line 11
        "    }\n"
        "}\n"
    )
    _write_cargo_project(tmp_path, lib)

    result, run = RustAdapter().test(str(tmp_path))

    assert result.outcome is Outcome.FAILED, run.combined_log
    assert result.is_error is True
    assert len(result.diagnostics) == 1, [d.to_dict() for d in result.diagnostics]
    d = result.diagnostics[0]
    assert d.file.replace("\\", "/") == "src/lib.rs"
    assert d.line == 11
    assert d.source == "nextest"
    assert "adds_wrong" in d.message


@pytest.mark.skipif(not _have_nextest(), reason="cargo nextest is not installed")
def test_rust_passing_suite_is_passed(tmp_path: Path):
    lib = (
        "pub fn add(a: i32, b: i32) -> i32 {\n"
        "    a + b\n"
        "}\n"
        "\n"
        "#[cfg(test)]\n"
        "mod tests {\n"
        "    use super::*;\n"
        "\n"
        "    #[test]\n"
        "    fn adds_right() {\n"
        "        assert_eq!(add(1, 1), 2);\n"
        "    }\n"
        "}\n"
    )
    _write_cargo_project(tmp_path, lib)

    result, run = RustAdapter().test(str(tmp_path))

    assert result.outcome is Outcome.PASSED, run.combined_log
    assert result.is_error is False
    assert result.diagnostics == []
