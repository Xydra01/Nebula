# hydrogen/tests/test_h04_page_block.py
"""Integration tests for Issue H04: Uniform Page Block Layout & Dual-Quantization Tagging."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
PAGE_BLOCK_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_test_page_block.exe"


def run_page_block_test(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(PAGE_BLOCK_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=30,
    )


def test_page_block_binary_exists():
    assert PAGE_BLOCK_EXE.exists(), f"Binary not found at: {PAGE_BLOCK_EXE}"


def test_page_block_help():
    proc = run_page_block_test(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--verbose" in proc.stdout


def test_page_block_execution_and_invariants():
    proc = run_page_block_test(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["page_size_bytes"] == 1288
    assert data["page_payload_bytes"] == 1280
    assert data["tokens_per_page"] == 16
    assert data["bytes_per_token"] == 80
    assert data["size_invariant_verified"] is True
    assert data["layout_offsets_verified"] is True
    assert data["dual_quant_policy_verified"] is True
    assert data["syntax_tokens_preserved"] is True


def test_quantization_fidelity_bounds():
    proc = run_page_block_test(["--json"])
    assert proc.returncode == 0

    data = json.loads(proc.stdout)
    assert data["q8_0"]["mse"] < 0.005
    assert data["q8_0"]["snr_db"] > 34.0
    assert data["fp8_e4m3"]["mse"] < 0.05
    assert data["fp8_e4m3"]["snr_db"] > 30.0


def test_page_block_formatted_output():
    proc = run_page_block_test([])
    assert proc.returncode == 0
    assert "UniformPageBlock Size:     1288 bytes" in proc.stdout
    assert "Dual-Quantization Hybrid Policy:" in proc.stdout
    assert "PASS (Sensitive Boundaries Protected)" in proc.stdout
    assert "PASS (Zero syntax degradation)" in proc.stdout
    assert "Overall Status:            PASS (All Invariants Verified)" in proc.stdout
