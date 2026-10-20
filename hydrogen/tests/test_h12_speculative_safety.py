# hydrogen/tests/test_h12_speculative_safety.py
"""Integration tests for Issue H12: Speculative Tool Parser & Mutating Turn-Finalization Barrier."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
SPECULATIVE_TEST_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_test_speculative_safety.exe"


def run_speculative_test(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(SPECULATIVE_TEST_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_speculative_safety_binary_exists():
    assert SPECULATIVE_TEST_EXE.exists(), f"Binary not found at: {SPECULATIVE_TEST_EXE}"


def test_speculative_safety_help():
    proc = run_speculative_test(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout


def test_speculative_safety_simulation_and_invariants():
    proc = run_speculative_test(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"

    ac = data["acceptance_criteria"]
    # Criterion 1: Read-only tools resolve file data before terminal token generation finishes
    assert ac["readonly_early_resolution_passed"] is True

    # Criterion 2: Mutating tools are 100% blocked until turn finalization and executor approval
    assert ac["mutating_barrier_100pct_blocked_passed"] is True

    # Criterion 3: Zero premature side-effects under simulated model hallucinations or mid-turn aborts
    assert ac["zero_premature_side_effects_passed"] is True

    metrics = data["metrics"]
    assert metrics["ro_lead_time_ms"] >= 10.0  # Lead time before terminal token emission
    assert metrics["ro_bytes_loaded"] > 0
    assert metrics["mutating_blocked_before_approval"] is True
    assert metrics["mutating_side_effects_before_approval"] is False
    assert metrics["abort_process_spawned"] is False
    assert metrics["premature_side_effects_count"] == 0


def test_speculative_safety_formatted_output():
    proc = run_speculative_test([])
    assert proc.returncode == 0
    stdout = proc.stdout
    assert "TEST CASE 1: READ-ONLY TOOL EARLY RESOLUTION (fs.read)" in stdout
    assert "TEST CASE 2: MUTATING TOOL TURN-FINALIZATION BARRIER (fs.write)" in stdout
    assert "TEST CASE 3: MODEL HALLUCINATION & MID-TURN ABORT (shell.run)" in stdout
    assert "[Criteria 1] Read-only tools resolve before terminal token: PASS" in stdout
    assert "[Criteria 2] Mutating tools 100% blocked until approval:    PASS" in stdout
    assert "[Criteria 3] Zero premature side-effects under abort:       PASS" in stdout
