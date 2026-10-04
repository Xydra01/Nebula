"""Minimal GGUF header reader.

The stock `gguf` package rejects PrismML's quant types (PTQ1_0 is type 143) and needs the whole
file to be present; this only parses metadata and tensor descriptors, so it works on any type and
on a partially downloaded file.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass
from pathlib import Path
from typing import BinaryIO

_SCALAR = {
    0: "<B",
    1: "<b",
    2: "<H",
    3: "<h",
    4: "<I",
    5: "<i",
    6: "<f",
    7: "<?",
    10: "<Q",
    11: "<q",
    12: "<d",
}
STRING, ARRAY = 8, 9


@dataclass
class KV:
    key: str
    vtype: int
    value: object
    raw: bytes  # the encoded value (type tag excluded), for byte-exact copying


@dataclass
class TensorInfo:
    name: str
    shape: list[int]
    ggml_type: int
    offset: int  # relative to the start of the data section


@dataclass
class Header:
    version: int
    kvs: dict[str, KV]
    tensors: list[TensorInfo]
    alignment: int
    data_start: int

    def get(self, key: str, default=None):
        kv = self.kvs.get(key)
        return kv.value if kv else default


def _read(f: BinaryIO, fmt: str):
    size = struct.calcsize(fmt)
    data = f.read(size)
    if len(data) != size:
        raise EOFError("truncated GGUF header")
    return struct.unpack(fmt, data)[0]


def _read_str(f: BinaryIO) -> str:
    n = _read(f, "<Q")
    return f.read(n).decode("utf-8", errors="replace")


def _read_value(f: BinaryIO, vtype: int, keep_array: bool):
    if vtype in _SCALAR:
        return _read(f, _SCALAR[vtype])
    if vtype == STRING:
        return _read_str(f)
    if vtype == ARRAY:
        etype = _read(f, "<I")
        n = _read(f, "<Q")
        if etype in _SCALAR and not keep_array:
            f.seek(struct.calcsize(_SCALAR[etype]) * n, 1)
            return f"<array of {n}>"
        items = [_read_value(f, etype, keep_array) for _ in range(n)]
        return items if keep_array or n <= 16 else f"<array of {n}>"
    raise ValueError(f"unknown GGUF value type {vtype}")


def read_header(path: str | Path, keep_arrays: bool = False) -> Header:
    with open(path, "rb") as f:
        if f.read(4) != b"GGUF":
            raise ValueError(f"{path} is not a GGUF file")
        version = _read(f, "<I")
        n_tensors = _read(f, "<Q")
        n_kv = _read(f, "<Q")
        kvs: dict[str, KV] = {}
        for _ in range(n_kv):
            key = _read_str(f)
            vtype = _read(f, "<I")
            start = f.tell()
            value = _read_value(f, vtype, keep_arrays)
            end = f.tell()
            f.seek(start)
            raw = f.read(end - start)
            kvs[key] = KV(key, vtype, value, raw)
        tensors = []
        for _ in range(n_tensors):
            name = _read_str(f)
            n_dims = _read(f, "<I")
            shape = [_read(f, "<Q") for _ in range(n_dims)]
            ggml_type = _read(f, "<I")
            offset = _read(f, "<Q")
            tensors.append(TensorInfo(name, shape, ggml_type, offset))
        alignment = kvs["general.alignment"].value if "general.alignment" in kvs else 32
        pos = f.tell()
        data_start = (pos + alignment - 1) // alignment * alignment
    return Header(version, kvs, tensors, alignment, data_start)
