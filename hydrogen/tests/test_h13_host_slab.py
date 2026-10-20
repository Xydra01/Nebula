# hydrogen/tests/test_h13_host_slab.py
"""Integration tests for Issue H13: Tiered Host Pinned Slab Allocator & 8 Sub-Arenas."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
BENCH_HOST_SLAB_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_bench_host_slab.exe"


def run_host_slab_bench(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(BENCH_HOST_SLAB_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_host_slab_binary_exists():
    assert BENCH_HOST_SLAB_EXE.exists(), f"Binary not found at: {BENCH_HOST_SLAB_EXE}"


def test_host_slab_help():
    proc = run_host_slab_bench(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout


def test_host_slab_simulation_and_invariants():
    proc = run_host_slab_bench(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"

    ac = data["acceptance_criteria"]
    # Criterion 1: Tiered fallback successfully initializes under simulated memory fragmentation
    assert ac["tiered_fallback_passed"] is True

    # Criterion 2: Address translation executes in < 1.0 ns using compile-time constants
    assert ac["address_translation_under_1ns_passed"] is True

    # Criterion 3: Zero false sharing or atomic bus contention across 32 concurrent threads
    assert ac["zero_false_sharing_and_reduction_passed"] is True

    metrics = data["metrics"]
    assert metrics["translation_ns_per_op"] < 1.0, f"Translation latency too high: {metrics['translation_ns_per_op']}"
    assert metrics["atomic_reduction_pct"] >= 98.0, f"Atomic reduction below target: {metrics['atomic_reduction_pct']}"
    assert metrics["tiered_alloc_latency_ns"] < 5.0, f"Alloc latency too high: {metrics['tiered_alloc_latency_ns']}"
    assert metrics["speedup_vs_contended"] > 10.0  # Significant speedup over contended baseline
    assert metrics["max_pinned_bytes"] == 4294967296  # Exactly 4 GiB cap
    assert metrics["total_slots"] == 3355392  # 16 slabs * 8 sub-arenas * 26,214 slots


def test_host_slab_formatted_output():
    proc = run_host_slab_bench([])
    assert proc.returncode == 0
    stdout = proc.stdout
    assert "Test 1 [Tiered Fallback Ladder]:  [PASS]" in stdout
    assert "Test 2 [Address Translation]:     [PASS]" in stdout
    assert "Test 3 [32 Threads Concurrency]:  [PASS]" in stdout
    assert "Final Status: ALL CRITERIA VERIFIED (PASS)" in stdout
