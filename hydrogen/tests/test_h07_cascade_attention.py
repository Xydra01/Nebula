# hydrogen/tests/test_h07_cascade_attention.py
"""Integration tests for Issue H07: FlashInfer SM 8.9 Cascade Attention Integration."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
CASCADE_BENCH_EXE = (
    REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_bench_cascade_attention.exe"
)


def run_cascade_bench(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(CASCADE_BENCH_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_cascade_bench_binary_exists():
    assert CASCADE_BENCH_EXE.exists(), f"Binary not found at: {CASCADE_BENCH_EXE}"


def test_cascade_bench_help():
    proc = run_cascade_bench(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--prefix" in proc.stdout
    assert "--leaf" in proc.stdout
    assert "--iters" in proc.stdout


def test_cascade_bench_batch_sweep_and_invariants():
    proc = run_cascade_bench(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["prefix_len"] == 4096
    assert data["leaf_len"] == 128
    assert data["head_dim"] == 128
    assert data["num_q_heads"] == 32
    assert data["num_kv_heads"] == 8

    # Acceptance Criteria Invariants
    assert data["numerical_tolerance_passed"] is True
    assert data["throughput_ge_500_tok_s"] is True
    assert data["speedup_ge_6x"] is True

    # B = 8 detailed metrics
    assert data["b8_latency_ms"] <= 14.08
    assert data["b8_throughput_tok_s"] >= 500.0
    assert data["b8_speedup_ratio"] >= 6.0
    assert data["b8_max_abs_error"] < 1e-4

    # Verify each batch size in sweep
    sweep = data["sweep"]
    assert len(sweep) == 4
    batch_sizes = [r["batch_size"] for r in sweep]
    assert batch_sizes == [1, 2, 4, 8]
    for r in sweep:
        assert r["max_abs_error"] < 1e-4
        assert r["throughput_tok_s"] > 0.0


def test_cascade_bench_custom_prefix_leaf():
    proc = run_cascade_bench(["--prefix", "2048", "--leaf", "64", "--iters", "10", "--json"])
    assert proc.returncode == 0

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["prefix_len"] == 2048
    assert data["leaf_len"] == 64
    assert data["numerical_tolerance_passed"] is True
    assert data["b8_max_abs_error"] < 1e-4


def test_cascade_bench_formatted_output():
    proc = run_cascade_bench([])
    assert proc.returncode == 0
    assert "Project Hydrogen: FlashInfer Cascade Attention SM 8.9" in proc.stdout
    assert "Shared Prefix=4096 tokens | Private Leaf=128 tokens" in proc.stdout
    assert "87.5% bandwidth reduction" in proc.stdout
    assert "Numerical Output Match:  PASS (Max err < 1e-4)" in proc.stdout
    assert "Aggregate Throughput:    PASS (>= 500 tok/s at B=8)" in proc.stdout
    assert "Batch Scaling Speedup:   PASS (>= 6.0x over B=1)" in proc.stdout
    assert "Overall Status:            PASS (All Invariants Verified)" in proc.stdout
