"""Graft the ProCreations MTP head from the PQ2_0+MTP GGUF onto the PTQ1_0 Bonsai GGUF.

The PQ2_0+MTP file is the unchanged PQ2_0 base plus one `blk.64` layer (the MTP head),
`block_count` 65 and `nextn_predict_layers` 1. Both bases share byte-identical
`prism.hadamard.*` metadata, so the head tensors are copied verbatim.

    uv run python -m nebula_bench.graft_mtp <ptq1_0.gguf> <pq2_0-mtp.gguf> <out.gguf>
"""

from __future__ import annotations

import shutil
import struct
import sys
from pathlib import Path

from nebula_bench.gguf_header import KV, Header, TensorInfo, read_header

# (elements per block, bytes per block) for the types the head uses.
TYPE_SIZE = {0: (1, 4), 1: (1, 2), 8: (32, 34)}
COPIED_KEYS_FROM_DONOR = ("qwen35.nextn_predict_layers",)


def _str(s: str) -> bytes:
    b = s.encode("utf-8")
    return struct.pack("<Q", len(b)) + b


def _nbytes(t: TensorInfo) -> int:
    per_block, block_bytes = TYPE_SIZE[t.ggml_type]
    n = 1
    for d in t.shape:
        n *= d
    return n // per_block * block_bytes


def _align(n: int, a: int) -> int:
    return (n + a - 1) // a * a


def graft(base_path: Path, donor_path: Path, out_path: Path) -> None:
    base, donor = read_header(base_path), read_header(donor_path)
    base_names = {t.name for t in base.tensors}
    head = [t for t in donor.tensors if t.name not in base_names]
    if not head or any(not t.name.startswith("blk.64.") for t in head):
        raise SystemExit(f"unexpected donor-only tensors: {[t.name for t in head]}")
    for key in (k for k in base.kvs if k.startswith("prism.hadamard.")):
        if base.kvs[key].raw != donor.kvs[key].raw:
            raise SystemExit(f"{key} differs between base and donor; the head would not match")

    kvs: dict[str, KV] = dict(base.kvs)
    kvs["qwen35.block_count"] = donor.kvs["qwen35.block_count"]
    for key in COPIED_KEYS_FROM_DONOR:
        kvs[key] = donor.kvs[key]
    name = "Ternary Bonsai 2 27B PTQ1_0 + ProCreations MTP head (grafted)"
    kvs["general.name"] = KV("general.name", 8, name, _str(name))

    base_data_len = base_path.stat().st_size - base.data_start
    a = base.alignment
    offset = _align(base_data_len, a)
    placed: list[tuple[TensorInfo, int]] = []
    for t in head:
        placed.append((t, offset))
        offset = _align(offset + _nbytes(t), a)

    header = bytearray(b"GGUF")
    header += struct.pack("<IQQ", base.version, len(base.tensors) + len(head), len(kvs))
    for kv in kvs.values():
        header += _str(kv.key) + struct.pack("<I", kv.vtype) + kv.raw
    for t in base.tensors:
        header += _tensor_info(t, t.offset)
    for t, off in placed:
        header += _tensor_info(t, off)
    header += b"\0" * (_align(len(header), a) - len(header))

    tmp = out_path.with_suffix(".partial")
    with open(tmp, "wb") as out, open(base_path, "rb") as src, open(donor_path, "rb") as dn:
        out.write(header)
        src.seek(base.data_start)
        shutil.copyfileobj(src, out, 64 * 1024 * 1024)
        data_start = len(header)
        for t, off in placed:
            pad = data_start + off - out.tell()
            out.write(b"\0" * pad)
            dn.seek(donor.data_start + t.offset)
            out.write(dn.read(_nbytes(t)))
    tmp.replace(out_path)
    _verify(out_path, base, head)


def _tensor_info(t: TensorInfo, offset: int) -> bytes:
    b = _str(t.name) + struct.pack("<I", len(t.shape))
    b += b"".join(struct.pack("<Q", d) for d in t.shape)
    return b + struct.pack("<IQ", t.ggml_type, offset)


def _verify(path: Path, base: Header, head: list[TensorInfo]) -> None:
    h = read_header(path)
    assert len(h.tensors) == len(base.tensors) + len(head)
    assert h.get("qwen35.block_count") == 65 and h.get("qwen35.nextn_predict_layers") == 1
    last = max(h.tensors, key=lambda t: t.offset)
    if last.ggml_type in TYPE_SIZE:
        assert h.data_start + last.offset + _nbytes(last) <= path.stat().st_size
    print(f"wrote {path} ({path.stat().st_size / 2**30:.2f} GiB, {len(h.tensors)} tensors)")


if __name__ == "__main__":
    graft(Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]))
