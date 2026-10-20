# hydrogen/tests/test_h03_forward.py
"""Integration tests for Issue H03: Single-Token Forward Smoke Test & VRAM Baseline Telemetry."""

import json
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
SMOKE_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_smoke_test.exe"
BONSAI_MODEL = Path(r"F:\Nebula\models\bonsai2-27b\Ternary-Bonsai-2-27B-PTQ1_0.gguf")
EMBEDDING_MODEL = Path(r"F:\Nebula\models\embedding\Qwen3-Embedding-0.6B-Q8_0.gguf")


def run_smoke_test(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(SMOKE_EXE)] + args
    return subprocess.run(cmd, capture_output=True, text=True, cwd=str(REPO_ROOT), timeout=60)


def test_smoke_test_binary_exists():
    assert SMOKE_EXE.exists(), f"Binary not found at: {SMOKE_EXE}"


def test_smoke_test_help():
    proc = run_smoke_test(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--model" in proc.stdout
    assert "--prompt" in proc.stdout
    assert "--json" in proc.stdout


@pytest.mark.skipif(not EMBEDDING_MODEL.exists(), reason="Qwen3 embedding model not found on F:")
def test_smoke_test_qwen3_embedding_forward():
    proc = run_smoke_test(["-m", str(EMBEDDING_MODEL), "-p", "The capital of France is", "--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["architecture"] == "qwen3"
    assert data["top_1_matches_reference"] is True
    assert "Paris" in data["top_tokens"][0]["token_str"]
    assert data["top_tokens"][0]["probability"] > 0.50
    assert data["vram_leaked_bytes"] == 0
    assert abs(data["vram_allocated_mib"] - 603.87) / 603.87 < 0.05
    assert data["forward_latency_ms"] < 50.0


@pytest.mark.skipif(not BONSAI_MODEL.exists(), reason="Bonsai 27B model not found on F:")
def test_smoke_test_bonsai27b_forward_fast():
    """Test Bonsai 27B forward pass with active tensors only."""
    args = ["-m", str(BONSAI_MODEL), "-p", "The capital of France is", "--no-alloc-full", "--json"]
    proc = run_smoke_test(args)
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["architecture"] == "qwen35"
    assert data["layer_count"] == 64
    assert data["embedding_dim"] == 5120
    assert data["top_1_matches_reference"] is True
    assert "Paris" in data["top_tokens"][0]["token_str"]
    assert data["top_tokens"][0]["probability"] > 0.50
    assert data["vram_leaked_bytes"] == 0
    assert data["forward_latency_ms"] < 50.0


@pytest.mark.skipif(not BONSAI_MODEL.exists(), reason="Bonsai 27B model not found on F:")
def test_smoke_test_bonsai27b_full_vram_allocation():
    """Verify full weight allocation (5,660 MiB) into VRAM conforming to Proof A bounds."""
    args = ["-m", str(BONSAI_MODEL), "-p", "The capital of France is", "--alloc-full", "--json"]
    proc = run_smoke_test(args)
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    # Conforms to ~5,660.56 MiB theoretical weights
    assert abs(data["vram_allocated_mib"] - 5660.56) / 5660.56 < 0.02
    assert data["vram_leaked_bytes"] == 0
    assert data["top_1_matches_reference"] is True


def test_smoke_test_missing_model_failure():
    proc = run_smoke_test(["-m", "non_existent_model_file.gguf", "--json"])
    assert proc.returncode != 0
    data = json.loads(proc.stdout)
    assert data["status"] == "FAIL"


@pytest.mark.skipif(not EMBEDDING_MODEL.exists(), reason="Qwen3 embedding model not found on F:")
def test_smoke_test_custom_prompt():
    args = ["-m", str(EMBEDDING_MODEL), "-p", "Hello world", "--no-alloc-full", "--json"]
    proc = run_smoke_test(args)
    assert proc.returncode == 0
    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["token_count"] > 0
    assert len(data["top_tokens"]) == 5
    assert data["vram_leaked_bytes"] == 0
