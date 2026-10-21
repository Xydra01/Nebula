"""Tests for Hydrogen Level 1 Harness² candidate evaluation (H18)."""

from __future__ import annotations

import sys
from pathlib import Path

# Add bench directory to sys.path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from hydrogen_harness_eval import (
    MAX_ALLOWED_HYDROGEN_LATENCY_SECS,
    MAX_ALLOWED_KV_CACHE_BYTES,
    SERIAL_LLAMA_CPP_LATENCY_SECS,
    HydrogenHarnessEvaluator,
)


def test_candidate_probes_generation() -> None:
    evaluator = HydrogenHarnessEvaluator()
    probes = evaluator.generate_candidate_probes(k=4)

    assert len(probes) == 8
    inv_probes = [p for p in probes if p.probe_type == "inv"]
    dom_probes = [p for p in probes if p.probe_type == "dom"]

    assert len(inv_probes) == 4
    assert len(dom_probes) == 4
    assert {p.probe_id for p in probes} == {
        "inv_0",
        "inv_1",
        "inv_2",
        "inv_3",
        "dom_0",
        "dom_1",
        "dom_2",
        "dom_3",
    }


def test_shared_kv_cache_memory_footprint() -> None:
    evaluator = HydrogenHarnessEvaluator(prefix_tokens=2048, tokens_per_probe=256)
    probes = evaluator.generate_candidate_probes(k=4)

    shared_kv_bytes = evaluator.calculate_shared_kv_cache_footprint(probes)
    serial_kv_bytes = evaluator.calculate_serial_kv_cache_footprint(probes)

    # Prefix sharing saves (8 - 1) * 2048 tokens = 14336 tokens of KV cache
    assert shared_kv_bytes < serial_kv_bytes
    # Must remain strictly below 3.5 GB
    assert shared_kv_bytes <= MAX_ALLOWED_KV_CACHE_BYTES
    # For 2048 prefix + 8*256 suffix = 4096 tokens * 64 KiB = 268,435,456 bytes (~256 MiB)
    assert shared_kv_bytes == (2048 + 8 * 256) * 64 * 1024


def test_batched_evaluation_execution() -> None:
    evaluator = HydrogenHarnessEvaluator(prefix_tokens=2048, tokens_per_probe=256)
    probes = evaluator.generate_candidate_probes(k=4)

    summary = evaluator.evaluate_batched(probes, simulate_time=True)

    assert summary.total_probes == 8
    assert len(summary.results) == 8
    assert all(r.verified for r in summary.results)

    # Execution time must be strictly under 40 seconds
    assert summary.wall_clock_seconds < MAX_ALLOWED_HYDROGEN_LATENCY_SECS
    # Speedup ratio over sequential llama.cpp (~235s) must be > 6.0x
    expected_speedup = SERIAL_LLAMA_CPP_LATENCY_SECS / summary.wall_clock_seconds
    assert summary.speedup_ratio >= 6.0
    assert summary.speedup_ratio == round(expected_speedup, 2)
    assert summary.kv_cache_gib < 3.5
