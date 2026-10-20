# hydrogen/tests/test_h09_dispatch_tiler.py
"""Integration tests for Issue H09: Templated Dispatch and WDDM Sub-5ms Kernel Clamping."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
DISPATCH_TILER_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_bench_dispatch_tiler.exe"


def run_dispatch_tiler(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(DISPATCH_TILER_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_dispatch_tiler_binary_exists():
    assert DISPATCH_TILER_EXE.exists(), f"Binary not found at: {DISPATCH_TILER_EXE}"


def test_dispatch_tiler_help():
    proc = run_dispatch_tiler(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--steps" in proc.stdout
    assert "--tokens" in proc.stdout
    assert "--batch" in proc.stdout
    assert "--quantum" in proc.stdout


def test_dispatch_tiler_sweep_and_invariants():
    proc = run_dispatch_tiler(["--json", "--steps", "500"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"

    # Acceptance Criteria Verification Invariants
    assert data["warp_efficiency_passed"] is True
    assert data["binary_size_passed"] is True
    assert data["driver_load_passed"] is True
    assert data["wddm_clamping_passed"] is True

    metrics = data["metrics"]
    # Criterion 1: Zero warp divergence in templated attention loop (>95% warp execution efficiency)
    assert metrics["templated_warp_efficiency_pct"] >= 95.0
    assert metrics["templated_warp_efficiency_pct"] == 100.0
    assert metrics["divergent_warp_efficiency_pct"] < 65.0
    assert metrics["speedup_ratio"] > 1.0

    # Criterion 2: Compiled binary size < 15.0 MB and sub-200ms driver load time
    assert metrics["binary_size_mb"] < 15.0
    assert metrics["driver_load_time_ms"] < 200.0

    # Criterion 3: Zero WDDM preemption stalls and max kernel duration < 5.0 ms
    assert metrics["max_kernel_duration_ms"] < 5.0
    assert metrics["preemption_stalls"] == 0
    assert metrics["total_steps"] == 500
    assert metrics["total_tiles_launched"] > 0

    # Forward Scheduler Pre-sorting: homogeneous sub-batches partitioned cleanly
    presort = data["presort_batch"]
    assert presort["input_requests"] == 6
    assert presort["homogeneous_subbatches"] == 3
    quant_types = [b["quant_type"] for b in presort["batches"]]
    assert "FP8_E4M3" in quant_types
    assert "Q8_0" in quant_types
    assert "FP16" in quant_types


def test_dispatch_tiler_custom_quantum():
    proc = run_dispatch_tiler(["--quantum", "3.0", "--tokens", "4096", "--batch", "2", "--steps", "50", "--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["wddm_clamping_passed"] is True
    assert data["metrics"]["max_kernel_duration_ms"] < 3.0
    assert data["metrics"]["preemption_stalls"] == 0


def test_dispatch_tiler_formatted_ascii():
    proc = run_dispatch_tiler(["--steps", "50"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"
    assert "Project Hydrogen: Templated Dispatch & WDDM Kernel Clamping SM 8.9" in proc.stdout
    assert "Forward Scheduler Pre-Sorting Demonstration:" in proc.stdout
    assert "Warp Execution Efficiency Benchmark (Divergent vs Templated):" in proc.stdout
    assert "Binary Size & Driver Initialization Latency:" in proc.stdout
    assert "WDDM Sub-5ms Clamping Invariant Sweep" in proc.stdout
    assert "ALL CHECKS PASSED [SUCCESS]" in proc.stdout
