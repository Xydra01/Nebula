# hydrogen/tests/test_h15_deadlock_and_tdr.py
"""Integration tests for Issue H15: 30s Zombie Auto-Reclaim & 500ms WDDM TDR Guard."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
TEST_DEADLOCK_AND_TDR_EXE = (
    REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_test_deadlock_and_tdr.exe"
)


def run_test_binary(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(TEST_DEADLOCK_AND_TDR_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_deadlock_and_tdr_binary_exists():
    assert TEST_DEADLOCK_AND_TDR_EXE.exists(), f"Binary not found at: {TEST_DEADLOCK_AND_TDR_EXE}"


def test_deadlock_and_tdr_help():
    proc = run_test_binary(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--fast-timeout" in proc.stdout


def test_deadlock_and_tdr_json_invariants():
    proc = run_test_binary(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    # Extract JSON from stdout (ignoring stderr logs)
    stdout_lines = proc.stdout.strip().split("\n")
    json_start = next(i for i, line in enumerate(stdout_lines) if line.strip().startswith("{"))
    json_text = "\n".join(stdout_lines[json_start:])
    data = json.loads(json_text)

    assert data["test_suite"] == "H15_wddm_tdr_guard_and_zombie_reclaim"
    assert data["status"] == "PASS"

    # Test 1: 3.5 GB High-Water Mark Pause Queue & 30s Zombie Process Auto-Reclaim
    t1 = data["test1_zombie_reclaim"]
    assert t1["passed"] is True
    assert t1["total_streams"] == 30
    assert t1["initial_swapped_streams"] == 22
    assert t1["initial_paused_streams"] == 8
    assert t1["initial_swapped_bytes_mib"] == 3520.0
    assert t1["hung_processes_count"] == 5
    assert t1["reclaimed_zombies_count"] == 5
    assert t1["reclaimed_bytes_mib"] == 800.0
    assert t1["unfrozen_streams_count"] == 5
    assert t1["remaining_paused_streams"] == 3
    assert t1["final_swapped_streams"] == 22
    assert t1["final_swapped_bytes_mib"] == 3520.0
    assert t1["dma_quiescence_enforced"] is True
    assert t1["write_after_free_prevented"] is True
    assert t1["termination_exit_code"] == "0xDEAD"

    # Test 2: 500 ms Non-Blocking WDDM TDR Guard
    t2 = data["test2_tdr_guard"]
    assert t2["passed"] is True
    assert t2["backstop_ms"] == 500
    assert t2["windows_tdr_delay_ms"] == 2000
    assert t2["safety_margin_factor"] == 4.0
    assert t2["fast_transfer_passed"] is True
    assert t2["simulated_stall_ms"] == 600
    assert t2["trip_detected_ms"] >= 500.0
    assert t2["restart_triggered"] is True
    assert t2["wddm_driver_crash_prevented"] is True


def test_deadlock_and_tdr_formatted_output():
    proc = run_test_binary([])
    assert proc.returncode == 0
    assert "HYDROGEN H15: 30s ZOMBIE RECLAIM & 500ms WDDM TDR GUARD VERIFICATION" in proc.stdout
    assert "Host Swap Pool Total Capacity : 4096 MiB (4 GiB)" in proc.stdout
    assert "High-Water Mark (87.5%)       : 3584 MiB (3.5 GiB)" in proc.stdout
    assert "Initial Swapped Streams       : 22 (3520 MiB)" in proc.stdout
    assert "Initial Paused in VRAM        : 8 (spill to NVMe avoided!)" in proc.stdout
    assert "Job Object Terminations       : 5 processes (exit code 0xDEAD)" in proc.stdout
    assert "Unfrozen Streams              : 5 streams" in proc.stdout
    assert "Remaining Paused Streams      : 3 (streams 27..29)" in proc.stdout
    assert "Test 1 Status                 : PASS" in proc.stdout
    assert "Subtest 2B (600 ms Stall)     : CAUGHT at" in proc.stdout
    assert "Hydrogen TDR Backstop Ceiling : 500 ms (4.0x safety factor)" in proc.stdout
    assert "WDDM Driver Reset (0x117)     : PREVENTED" in proc.stdout
    assert "Test 2 Status                 : PASS" in proc.stdout
    assert "ALL VERIFICATION CHECKS PASSED (H15)" in proc.stdout
