# hydrogen/tests/test_h06_page_pool.py
"""Integration tests for Issue H06: Lock-Free Page Pool Allocator & VRAM Compaction Sweeper."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
PAGE_POOL_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_stress_test_page_pool.exe"


def run_page_pool_test(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(PAGE_POOL_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_page_pool_binary_exists():
    assert PAGE_POOL_EXE.exists(), f"Binary not found at: {PAGE_POOL_EXE}"


def test_page_pool_help():
    proc = run_page_pool_test(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--ops" in proc.stdout
    assert "--threads" in proc.stdout


def test_page_pool_stress_test_and_invariants():
    proc = run_page_pool_test(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["pool_capacity_pages"] == 65536
    assert data["total_operations"] == 500000
    assert data["concurrent_threads"] == 16
    assert data["total_allocated_pages"] == 500000
    assert data["total_freed_pages"] == 500000
    assert data["ns_per_page_event"] < 50.0
    assert data["latency_under_50ns"] is True
    assert data["remaining_allocated_pages"] == 0
    assert data["zero_leakage_verified"] is True
    assert data["double_free_count"] == 0
    assert data["post_compaction_fragmentation"] == 0.0
    assert data["compaction_verified"] is True


def test_page_pool_custom_ops_and_threads():
    proc = run_page_pool_test(["--ops", "50000", "--threads", "8", "--json"])
    assert proc.returncode == 0

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["total_operations"] == 50000
    assert data["concurrent_threads"] == 8
    assert data["total_allocated_pages"] == 50000
    assert data["zero_leakage_verified"] is True
    assert data["double_free_count"] == 0


def test_page_pool_formatted_output():
    proc = run_page_pool_test([])
    assert proc.returncode == 0
    assert "Avg Latency per Event:" in proc.stdout
    assert "Page Leakage / Retained: 0 pages - PASS (Zero Leaks)" in proc.stdout
    assert "Double-Free Errors:      0 - PASS" in proc.stdout
    assert "Post-Compaction Frag:    0.0% (Target: 0.0%) - PASS" in proc.stdout
    assert "Overall Status:            PASS (All Invariants Verified)" in proc.stdout
