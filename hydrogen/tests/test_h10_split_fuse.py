# hydrogen/tests/test_h10_split_fuse.py
"""Integration tests for Issue H10: Split-Fuse Continuous Batching Scheduler."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
BATCHER_BENCH_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_bench_split_fuse_batcher.exe"


def run_batcher_bench(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(BATCHER_BENCH_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_split_fuse_binary_exists():
    assert BATCHER_BENCH_EXE.exists(), f"Binary not found at: {BATCHER_BENCH_EXE}"


def test_split_fuse_help():
    proc = run_batcher_bench(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--streams" in proc.stdout
    assert "--prefill" in proc.stdout
    assert "--chunk" in proc.stdout
    assert "--pre-steps" in proc.stdout
    assert "--post-steps" in proc.stdout


def test_split_fuse_simulation_and_invariants():
    proc = run_batcher_bench(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"

    # Acceptance Criteria Verification Invariants
    assert data["zero_stutter_passed"] is True
    assert data["max_chunk_clamped_passed"] is True
    assert data["assembly_overhead_passed"] is True
    assert data["prefill_duration_passed"] is True

    metrics = data["metrics"]
    assert metrics["decode_streams"] == 8
    assert metrics["prefill_tokens"] == 1500
    assert metrics["max_prefill_chunk_tokens"] == 64

    # Criterion 1: Maximum prefill chunk strictly clamped to 64 tokens
    assert metrics["max_prefill_chunk_observed"] <= 64
    assert metrics["max_prefill_chunk_observed"] == 64

    # Criterion 2: Zero perceptible decode stutter during massive prefill ingestion
    # Step latency <= 15.5 ms, decode jitter <= 10.0%, 1500 tokens in 24 iterations (<300 ms)
    assert metrics["max_decode_step_latency_ms"] <= 15.50
    assert metrics["decode_jitter_pct"] <= 10.0
    assert metrics["prefill_iterations"] == 24
    assert metrics["prefill_total_duration_ms"] < 300.0

    # Criterion 3: Continuous batch assembly overhead < 15.0 us per forward step
    assert metrics["avg_assembly_overhead_us"] < 15.0
    assert metrics["max_assembly_overhead_us"] < 15.0

    # Verification against monolithic prefill freeze
    assert metrics["monolithic_prefill_latency_ms"] > 200.0
    assert metrics["monolithic_jitter_pct"] > 1000.0


def test_split_fuse_custom_parameters():
    proc = run_batcher_bench(
        ["--streams", "4", "--prefill", "512", "--chunk", "64", "--pre-steps", "10", "--post-steps", "10", "--json"]
    )
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    metrics = data["metrics"]
    assert metrics["decode_streams"] == 4
    assert metrics["prefill_tokens"] == 512
    assert metrics["prefill_iterations"] == 8  # 512 / 64 = 8
    assert metrics["max_prefill_chunk_observed"] <= 64
    assert metrics["decode_jitter_pct"] <= 10.0
    assert metrics["max_decode_step_latency_ms"] <= 15.50
    assert metrics["avg_assembly_overhead_us"] < 15.0


def test_split_fuse_formatted_ascii():
    proc = run_batcher_bench([])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"
    assert "Project Hydrogen: Split-Fuse Continuous Batching Scheduler SM 8.9" in proc.stdout
    assert "Workload Configuration:" in proc.stdout
    assert "Decode Step Latency & UI Stutter Evaluation:" in proc.stdout
    assert "Monolithic vs Split-Fuse Jitter Comparison:" in proc.stdout
    assert "Continuous Batch Assembly Overhead" in proc.stdout
    assert "ALL CHECKS PASSED [SUCCESS]" in proc.stdout
