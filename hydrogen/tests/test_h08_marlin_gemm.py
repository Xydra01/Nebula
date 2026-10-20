# hydrogen/tests/test_h08_marlin_gemm.py
"""Integration tests for Issue H08: Marlin & CUTLASS Quantized GEMM Pipeline."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
MARLIN_BENCH_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_bench_marlin_gemm.exe"


def run_marlin_bench(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(MARLIN_BENCH_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_marlin_bench_binary_exists():
    assert MARLIN_BENCH_EXE.exists(), f"Binary not found at: {MARLIN_BENCH_EXE}"


def test_marlin_bench_help():
    proc = run_marlin_bench(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--k" in proc.stdout
    assert "--n" in proc.stdout
    assert "--group" in proc.stdout
    assert "--iters" in proc.stdout
    assert "--warmups" in proc.stdout


def test_marlin_bench_sweep_and_invariants():
    proc = run_marlin_bench(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["K"] == 4096
    assert data["N"] == 4096
    assert data["group_size"] == 128

    # Acceptance Criteria Verification Invariants
    assert data["repack_under_1s"] is True
    assert data["repack_time_ms"] < 1000.0  # < 1.0s
    assert data["numerical_match_passed"] is True
    assert data["latency_le_0_15ms_passed"] is True
    assert data["speedup_gt_1_8x_passed"] is True
    assert data["bandwidth_gt_80pct_passed"] is True

    # Sweep evaluation across M in {1, 2, 4, 8}
    sweep = data["sweep"]
    assert len(sweep) == 4
    batch_sizes = [r["M"] for r in sweep]
    assert batch_sizes == [1, 2, 4, 8]

    peak_bandwidth_seen = 0.0
    for r in sweep:
        # Precision invariant
        assert r["max_abs_error"] < 1e-3
        # Latency invariant (<= 0.15 ms)
        assert r["marlin_latency_ms"] <= 0.15
        # Baseline speedup invariant (> 1.8x)
        assert r["speedup_ratio"] > 1.8
        # Memory bandwidth track
        if r["bandwidth_util_pct"] > peak_bandwidth_seen:
            peak_bandwidth_seen = r["bandwidth_util_pct"]

    # Peak weight read saturation exceeds 80% (RTX 4070 504 GB/s)
    assert peak_bandwidth_seen >= 80.0


def test_marlin_bench_custom_dims():
    proc = run_marlin_bench(["--k", "2048", "--n", "2048", "--iters", "10", "--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["K"] == 2048
    assert data["N"] == 2048
    assert data["repack_under_1s"] is True
    assert data["numerical_match_passed"] is True
    assert data["latency_le_0_15ms_passed"] is True


def test_marlin_bench_formatted_output():
    proc = run_marlin_bench([])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"
    assert "Project Hydrogen: Marlin & CUTLASS Quantized GEMM SM 8.9" in proc.stdout
    assert "Weight Repacking (<1.0s):  PASS" in proc.stdout
    assert "GEMM Speedup (>1.8x):      PASS" in proc.stdout
    assert "Execution Time (<=0.15ms): PASS" in proc.stdout
    assert "Memory Bus Saturation (>80%): PASS" in proc.stdout
    assert "Numerical Accuracy (<1e-3):   PASS" in proc.stdout
    assert "Overall Status:            PASS (All Invariants Verified)" in proc.stdout
