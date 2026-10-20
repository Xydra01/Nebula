# hydrogen/tests/test_h14_dma_swapper.py
"""Integration tests for Issue H14: Asynchronous PCIe 4.0 DMA Swap Stream & Three-Phase Cancellation Barrier."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
BENCH_DMA_SWAPPER_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_bench_dma_swapper.exe"


def run_dma_swapper_bench(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(BENCH_DMA_SWAPPER_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_dma_swapper_binary_exists():
    assert BENCH_DMA_SWAPPER_EXE.exists(), f"Binary not found at: {BENCH_DMA_SWAPPER_EXE}"


def test_dma_swapper_help():
    proc = run_dma_swapper_bench(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--force-cpu" in proc.stdout
    assert "--iterations" in proc.stdout


def test_dma_swapper_full_swap_and_invariants():
    proc = run_dma_swapper_bench(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["all_criteria_met"] is True
    assert data["data_integrity_verified"] is True
    assert data["final_engine_state"] == "RUNNABLE"
    assert data["concurrent_compute_pass"] is True

    # Geometry checks: 160 MiB leaf KV cache divided into 80 chunks of 2 MiB
    assert data["total_swap_bytes"] == 160 * 1024 * 1024
    assert data["batch_size_bytes"] == 2 * 1024 * 1024
    assert data["total_batches"] == 80

    # Criterion 1: Eviction and restoration <= 7.5 ms normalized PCIe 4.0 latency
    assert data["pcie4_normalized_eviction_ms"] <= 7.5, (
        f"Eviction normalized latency exceeded ceiling: {data['pcie4_normalized_eviction_ms']} ms"
    )
    assert data["pcie4_normalized_restoration_ms"] <= 7.5, (
        f"Restoration normalized latency exceeded ceiling: {data['pcie4_normalized_restoration_ms']} ms"
    )

    # Criterion 2: Safe mid-flight cancellation barrier recovering in < 1.0 ms
    assert data["cancellation_recovery_ms"] < 1.0, (
        f"Cancellation recovery exceeded 1.0 ms ceiling: {data['cancellation_recovery_ms']} ms"
    )
    assert data["cancellation_recovery_max_ms"] < 1.0, (
        f"Max cancellation recovery exceeded 1.0 ms ceiling: {data['cancellation_recovery_max_ms']} ms"
    )
    assert 0 < data["batches_evicted_before_cancel"] < data["total_batches"], (
        f"Expected partial batch transfer before cancellation, got: {data['batches_evicted_before_cancel']}"
    )


def test_dma_swapper_cpu_emulation():
    proc = run_dma_swapper_bench(["--force-cpu", "--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["all_criteria_met"] is True
    assert data["data_integrity_verified"] is True
    assert data["final_engine_state"] == "RUNNABLE"
    assert data["concurrent_compute_pass"] is True
    assert data["cancellation_recovery_ms"] < 1.0
    assert 0 < data["batches_evicted_before_cancel"] < data["total_batches"]


def test_dma_swapper_formatted_output():
    proc = run_dma_swapper_bench([])
    assert proc.returncode == 0
    assert "Hydrogen H14 Benchmark" in proc.stdout
    assert "ALL ACCEPTANCE CRITERIA MET [PASS]" in proc.stdout
    assert "100% BIT-EXACT MATCH [PASS]" in proc.stdout
    assert "[PASS < 1.0 ms]" in proc.stdout
    assert "Compute Stream Non-Blocking:        ACTIVE (ZERO STALLS OBSERVED) [PASS]" in proc.stdout
