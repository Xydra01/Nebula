# hydrogen/tests/test_h05_radix_tree.py
"""Integration tests for Issue H05: Radix-Tree Paged KV Cache & Copy-on-Write Branching."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
RADIX_TREE_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_test_radix_tree.exe"


def run_radix_tree_test(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(RADIX_TREE_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=30,
    )


def test_radix_tree_binary_exists():
    assert RADIX_TREE_EXE.exists(), f"Binary not found at: {RADIX_TREE_EXE}"


def test_radix_tree_help():
    proc = run_radix_tree_test(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--verbose" in proc.stdout


def test_radix_tree_invariants_and_refcounts():
    proc = run_radix_tree_test(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["root_prefix_tokens"] == 4096
    assert data["branch_divergent_tokens"] == 2048
    assert data["num_concurrent_streams"] == 8
    assert data["root_node_ref_count"] == 8
    assert data["expected_root_ref_count"] == 8
    assert data["root_pages_allocated"] == 256
    assert data["total_physical_pages"] == 1280
    assert data["naive_physical_pages"] == 3072
    assert data["pages_saved"] == 1792
    assert abs(data["page_savings_ratio"] - 0.5833) < 0.001
    assert data["vram_physical_bytes"] == 1648640
    assert data["vram_naive_bytes"] == 3956736
    assert data["vram_saved_bytes"] == 2308096
    assert data["prefix_sharing_verified"] is True
    assert data["concurrent_branching_verified"] is True


def test_radix_tree_formatted_output():
    proc = run_radix_tree_test([])
    assert proc.returncode == 0
    assert "Shared Root Pages:       256 pages (ref_count=8) - PASS" in proc.stdout
    assert "Total Tree Pages:        1280 pages (Target: 1280) - PASS" in proc.stdout
    assert "VRAM Savings:            58.33%" in proc.stdout
    assert "Root Pages Refcount:     PASS (All 256 pages ref=8)" in proc.stdout
    assert "Leaf Pages Refcount:     PASS (All 1024 pages ref=1)" in proc.stdout
    assert "Overall Status:            PASS (All Invariants Verified)" in proc.stdout
