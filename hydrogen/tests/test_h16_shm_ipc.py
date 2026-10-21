# hydrogen/tests/test_h16_shm_ipc.py
"""Integration tests for Issue H16: Win32 Named Shared Memory IPC & Zero-Copy Token Signaling."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
BENCH_SHM_IPC_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_bench_shm_ipc.exe"


def run_benchmark_binary(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(BENCH_SHM_IPC_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_shm_ipc_binary_exists():
    assert BENCH_SHM_IPC_EXE.exists(), f"Binary not found at: {BENCH_SHM_IPC_EXE}"


def test_shm_ipc_help():
    proc = run_benchmark_binary(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--iterations" in proc.stdout
    assert "--latency-iters" in proc.stdout


def test_shm_ipc_json_invariants():
    proc = run_benchmark_binary(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    # Extract JSON from stdout
    stdout_lines = proc.stdout.strip().split("\n")
    json_start = next(i for i, line in enumerate(stdout_lines) if line.strip().startswith("{"))
    json_text = "\n".join(stdout_lines[json_start:])
    data = json.loads(json_text)

    assert data["benchmark"] == "H16_win32_shared_memory_ipc"
    assert data["status"] == "PASS"
    assert data["all_criteria_met"] is True

    # Shared Memory Geometry: 64 MiB total, 16 MiB request, 48 MiB response
    assert data["shm_total_capacity_bytes"] == 64 * 1024 * 1024
    assert data["request_queue_capacity_bytes"] == 16 * 1024 * 1024
    assert data["response_queue_capacity_bytes"] == 48 * 1024 * 1024

    # Latency Benchmark: 10,000 iterations, p99 < 50 us ceiling
    latency = data["latency_benchmark"]
    assert latency["iterations"] == 10000
    assert latency["passed"] is True
    assert latency["p99_latency_us"] < 50.0, (
        f"p99 latency {latency['p99_latency_us']} us exceeded 50 us ceiling"
    )
    assert latency["p50_latency_us"] < 25.0, (
        f"p50 latency {latency['p50_latency_us']} us abnormally high"
    )
    assert latency["speedup_vs_http_sse"] > 90.0, (
        f"Speedup vs HTTP {latency['speedup_vs_http_sse']}x did not meet 90x improvement target"
    )

    # Streaming Benchmark: 100,000 tokens streamed with zero dropped or corrupted frames
    streaming = data["streaming_benchmark"]
    assert streaming["tokens_streamed"] == 100000
    assert streaming["tokens_received"] == 100000
    assert streaming["zero_copy_verified"] is True
    assert streaming["zero_dropped_frames"] is True
    assert streaming["zero_corrupted_frames"] is True
    assert streaming["throughput_tokens_per_sec"] > 500000.0, (
        f"Throughput {streaming['throughput_tokens_per_sec']} tokens/s below expected threshold"
    )
    assert streaming["passed"] is True


def test_shm_ipc_formatted_output():
    proc = run_benchmark_binary([])
    assert proc.returncode == 0
    assert "HYDROGEN H16: WIN32 SHARED MEMORY IPC PROTOCOL BENCHMARK" in proc.stdout
    assert "Total Shared Memory Capacity  : 64 MiB" in proc.stdout
    assert "Request Queue (Daemon->Engine): 16 MiB" in proc.stdout
    assert "Response Queue (Engine->Daemon: 48 MiB" in proc.stdout
    assert "PASS < 50 us" in proc.stdout
    assert "Dropped / Corrupted Frames    : ZERO (100% bit-exact match)" in proc.stdout
    assert "Active Tool Process Atomic Hook: VERIFIED" in proc.stdout
    assert "Win32 Handle Cleanup          : ZERO LEAKS" in proc.stdout
    assert "ALL ACCEPTANCE CRITERIA MET (H16)" in proc.stdout
