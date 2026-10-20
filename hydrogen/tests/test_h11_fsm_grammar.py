# hydrogen/tests/test_h11_fsm_grammar.py
"""Integration tests for Issue H11: GPU-Fused FSM Tool Grammar Bitmask Sampler."""

import json
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
FSM_BENCH_EXE = REPO_ROOT / "hydrogen" / "build" / "bin" / "hydrogen_bench_fsm_sampler.exe"


def run_fsm_bench(args: list[str]) -> subprocess.CompletedProcess:
    cmd = [str(FSM_BENCH_EXE)] + args
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )


def test_fsm_sampler_binary_exists():
    assert FSM_BENCH_EXE.exists(), f"Binary not found at: {FSM_BENCH_EXE}"


def test_fsm_sampler_help():
    proc = run_fsm_bench(["--help"])
    assert proc.returncode == 0
    assert "Usage:" in proc.stdout
    assert "--json" in proc.stdout
    assert "--tool-calls" in proc.stdout
    assert "--latency-iters" in proc.stdout
    assert "--sample-output" in proc.stdout


def test_fsm_sampler_simulation_and_invariants():
    proc = run_fsm_bench(["--json"])
    assert proc.returncode == 0, f"Process failed: {proc.stderr}\n{proc.stdout}"

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"

    # Acceptance Criteria Verification Invariants
    ac = data["acceptance_criteria"]
    assert ac["sub_2us_validation_latency_passed"] is True
    assert ac["zero_stream_sync_passed"] is True
    assert ac["schema_conformity_100pct_passed"] is True

    metrics = data["metrics"]
    assert metrics["vocab_size"] == 152064
    assert metrics["mask_words"] == 4752
    assert metrics["sram_footprint_bytes"] == 19008  # Strictly 19,008 bytes (~19.0 KiB)

    # Criterion 1: Schema validation latency < 2.0 us on GPU
    assert metrics["gpu_validation_latency_us"] < 2.0
    assert metrics["speedup_factor"] >= 1000.0  # Speedup over CPU GBNF baseline (2.4 ms)

    # Criterion 2: 0 CPU-GPU stream synchronizations during tool argument generation
    assert metrics["stream_sync_count"] == 0

    # Criterion 3: 100% JSON schema conformity across tool invocations (0 syntax errors)
    assert metrics["total_tool_calls_generated"] >= 10000
    assert metrics["json_syntax_errors"] == 0
    assert metrics["schema_conformity_pct"] == 100.0


def test_fsm_sampler_json_schema_parsing():
    proc = run_fsm_bench(["--json", "--sample-output", "10"])
    assert proc.returncode == 0

    data = json.loads(proc.stdout)
    sample_calls = data["sample_tool_calls"]
    assert len(sample_calls) >= 5

    valid_tool_names = {"fs.read", "shell.run", "task.finish", "memory.lookup"}

    for call_str in sample_calls:
        # Strict parsing with Python standard JSON parser
        parsed = json.loads(call_str)
        assert isinstance(parsed, dict)
        assert "name" in parsed
        tool_name = parsed["name"]
        assert tool_name in valid_tool_names

        if tool_name == "fs.read":
            assert "path" in parsed
            assert isinstance(parsed["path"], str)
            assert len(parsed["path"]) > 0
        elif tool_name == "shell.run":
            assert "command" in parsed
            assert isinstance(parsed["command"], str)
            assert "args" in parsed
            assert isinstance(parsed["args"], list)
            assert len(parsed["args"]) > 0
            for arg in parsed["args"]:
                assert isinstance(arg, str)
        elif tool_name == "task.finish":
            assert "result" in parsed
            assert isinstance(parsed["result"], str)
            assert len(parsed["result"]) > 0
        elif tool_name == "memory.lookup":
            assert "query" in parsed
            assert isinstance(parsed["query"], str)
            assert "limit" in parsed
            assert isinstance(parsed["limit"], int)
            assert parsed["limit"] > 0


def test_fsm_sampler_parameter_sweep():
    proc = run_fsm_bench(["--json", "--tool-calls", "2000", "--latency-iters", "500"])
    assert proc.returncode == 0

    data = json.loads(proc.stdout)
    assert data["status"] == "PASS"
    assert data["metrics"]["total_tool_calls_generated"] == 2000
    assert data["metrics"]["json_syntax_errors"] == 0
    assert data["metrics"]["schema_conformity_pct"] == 100.0
    assert data["metrics"]["gpu_validation_latency_us"] < 2.0
