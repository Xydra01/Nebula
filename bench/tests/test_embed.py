"""Pure helpers of the embedding check (no server needed)."""

from __future__ import annotations

import math

from nebula_bench.embed import QUERIES, _dot, _normalize, code_chunks


def test_code_chunks_cover_query_targets() -> None:
    chunks = code_chunks()
    for want in QUERIES.values():
        assert want in chunks, want
    assert chunks["free_port"].startswith("def free_port")
    assert chunks["Server._wait_healthy"].lstrip().startswith("def _wait_healthy")


def test_normalize_and_dot() -> None:
    v = _normalize([3.0, 4.0])
    assert math.isclose(_dot(v, v), 1.0)
    assert v == [0.6, 0.8]
    assert _normalize([0.0, 0.0]) == [0.0, 0.0]
