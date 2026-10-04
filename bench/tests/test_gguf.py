"""GGUF header reader and MTP graft, on tiny synthetic files."""

from __future__ import annotations

import struct
from pathlib import Path

import pytest

from nebula_bench.gguf_header import read_header
from nebula_bench.graft_mtp import graft

U32, STR, F32 = 4, 8, 0
ALIGN = 32


def _str(s: str) -> bytes:
    b = s.encode()
    return struct.pack("<Q", len(b)) + b


def write_gguf(path: Path, kvs: dict[str, object], tensors: dict[str, bytes]) -> None:
    """Writes f32 1-D tensors; `kvs` values are ints (uint32) or strings."""
    out = bytearray(b"GGUF") + struct.pack("<IQQ", 3, len(tensors), len(kvs))
    for key, value in kvs.items():
        if isinstance(value, str):
            out += _str(key) + struct.pack("<I", STR) + _str(value)
        else:
            out += _str(key) + struct.pack("<II", U32, value)
    offset, offsets = 0, []
    for name, data in tensors.items():
        offsets.append(offset)
        out += _str(name) + struct.pack("<IQIQ", 1, len(data) // 4, F32, offset)
        offset += -(-len(data) // ALIGN) * ALIGN
    out += b"\0" * (-len(out) % ALIGN)
    start = len(out)
    for off, data in zip(offsets, tensors.values(), strict=True):
        out += b"\0" * (start + off - len(out)) + data
    path.write_bytes(bytes(out))


def floats(*xs: float) -> bytes:
    return struct.pack(f"<{len(xs)}f", *xs)


BASE_KVS = {
    "general.alignment": ALIGN,
    "general.name": "base",
    "qwen35.block_count": 64,
    "prism.hadamard.seed": 7,
}


def test_read_header(tmp_path: Path) -> None:
    p = tmp_path / "m.gguf"
    write_gguf(p, BASE_KVS, {"blk.0.w": floats(1, 2, 3), "blk.1.w": floats(4)})
    h = read_header(p)
    assert h.version == 3
    assert h.get("general.name") == "base"
    assert h.get("qwen35.block_count") == 64
    assert h.get("missing", "dflt") == "dflt"
    assert [(t.name, t.shape, t.ggml_type) for t in h.tensors] == [
        ("blk.0.w", [3], F32),
        ("blk.1.w", [1], F32),
    ]
    assert h.data_start % ALIGN == 0
    raw = p.read_bytes()
    t1 = h.tensors[1]
    assert raw[h.data_start + t1.offset : h.data_start + t1.offset + 4] == floats(4)


def test_read_header_rejects_non_gguf(tmp_path: Path) -> None:
    p = tmp_path / "x.bin"
    p.write_bytes(b"NOPE" + b"\0" * 32)
    with pytest.raises(ValueError, match="not a GGUF"):
        read_header(p)


def test_read_header_truncated(tmp_path: Path) -> None:
    p = tmp_path / "t.gguf"
    write_gguf(p, BASE_KVS, {"blk.0.w": floats(1)})
    p.write_bytes(p.read_bytes()[:20])
    with pytest.raises(EOFError):
        read_header(p)


def _donor_kvs(**over: object) -> dict[str, object]:
    return {
        **BASE_KVS,
        "general.name": "donor",
        "qwen35.block_count": 65,
        "qwen35.nextn_predict_layers": 1,
        **over,
    }


def test_graft_appends_head(tmp_path: Path) -> None:
    base, donor, out = tmp_path / "b.gguf", tmp_path / "d.gguf", tmp_path / "o.gguf"
    write_gguf(base, BASE_KVS, {"blk.0.w": floats(1, 2, 3)})
    write_gguf(
        donor,
        _donor_kvs(),
        {"blk.0.w": floats(9, 9, 9), "blk.64.a": floats(5, 6), "blk.64.b": floats(7)},
    )
    graft(base, donor, out)

    h = read_header(out)
    assert h.get("qwen35.block_count") == 65
    assert h.get("qwen35.nextn_predict_layers") == 1
    assert "grafted" in h.get("general.name")
    raw = out.read_bytes()

    def data(name: str, n: int) -> bytes:
        t = next(t for t in h.tensors if t.name == name)
        return raw[h.data_start + t.offset : h.data_start + t.offset + 4 * n]

    assert data("blk.0.w", 3) == floats(1, 2, 3)  # base weights, not the donor's
    assert data("blk.64.a", 2) == floats(5, 6)
    assert data("blk.64.b", 1) == floats(7)
    assert not out.with_suffix(".partial").exists()


def test_graft_refuses_mismatched_hadamard(tmp_path: Path) -> None:
    base, donor = tmp_path / "b.gguf", tmp_path / "d.gguf"
    write_gguf(base, BASE_KVS, {"blk.0.w": floats(1)})
    write_gguf(donor, _donor_kvs(**{"prism.hadamard.seed": 8}), {"blk.64.a": floats(1)})
    with pytest.raises(SystemExit, match="differs"):
        graft(base, donor, tmp_path / "o.gguf")


def test_graft_refuses_unexpected_tensors(tmp_path: Path) -> None:
    base, donor = tmp_path / "b.gguf", tmp_path / "d.gguf"
    write_gguf(base, BASE_KVS, {"blk.0.w": floats(1)})
    write_gguf(donor, _donor_kvs(), {"blk.0.w": floats(1), "output.extra": floats(1)})
    with pytest.raises(SystemExit, match="unexpected"):
        graft(base, donor, tmp_path / "o.gguf")
