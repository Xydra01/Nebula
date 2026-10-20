import json
import subprocess
from pathlib import Path

import pytest

HYDROGEN_DIR = Path(__file__).resolve().parent.parent
INSPECT_EXE = HYDROGEN_DIR / "build" / "bin" / "hydrogen_inspect_model.exe"


def run_inspect(model_path: str) -> dict:
    assert INSPECT_EXE.is_file(), f"Binary not found: {INSPECT_EXE}. Run build.ps1 first."
    cmd = [str(INSPECT_EXE), model_path, "--json"]
    result = subprocess.run(cmd, capture_output=True, text=True)
    assert result.returncode == 0, (
        f"Inspection failed ({result.returncode}):\n{result.stderr}\n{result.stdout}"
    )
    return json.loads(result.stdout)


def test_h02_inspect_kv_bias_model():
    model_path = r"F:\Nebula\models\bonsai2-27b\Ternary-Bonsai-2-27B-PTQ1_0-kv-bias.gguf"
    if not Path(model_path).is_file():
        pytest.skip(f"Model file not present: {model_path}")

    data = run_inspect(model_path)
    assert data["status"] == "PASS"
    assert data["version"] == 3
    assert data["tensor_count"] == 16
    assert data["metadata_kv_count"] == 2
    assert data["alignment"] == 32
    assert data["quantization_breakdown"]["F32"] == 16
    assert data["parse_duration_ms"] < 100.0


def test_h02_inspect_qwen_embedding_model():
    model_path = r"F:\Nebula\models\embedding\Qwen3-Embedding-0.6B-Q8_0.gguf"
    if not Path(model_path).is_file():
        pytest.skip(f"Model file not present: {model_path}")

    data = run_inspect(model_path)
    assert data["status"] == "PASS"
    assert data["version"] == 3
    assert data["architecture"] == "qwen3"
    assert data["layer_count"] == 28
    assert data["tensor_count"] == 310
    assert data["embedding_length"] == 1024
    assert data["head_count"] == 16
    assert data["head_count_kv"] == 8
    assert data["parse_duration_ms"] < 100.0


def test_h02_inspect_bonsai_27b_model():
    model_path = r"F:\Nebula\models\bonsai2-27b\Ternary-Bonsai-2-27B-PTQ1_0.gguf"
    if not Path(model_path).is_file():
        pytest.skip(f"Model file not present: {model_path}")

    data = run_inspect(model_path)
    assert data["status"] == "PASS"
    assert data["version"] == 3
    assert data["architecture"] == "qwen35"
    assert data["layer_count"] == 64
    assert data["tensor_count"] == 851
    assert data["context_length"] == 262144
    assert data["embedding_length"] == 5120
    assert data["head_count"] == 24
    assert data["head_count_kv"] == 4
    assert data["file_size_bytes"] == 5946648928
    assert data["parse_duration_ms"] < 100.0


def test_h02_invalid_file_handling():
    cmd = [str(INSPECT_EXE), "non_existent_file.gguf", "--json"]
    result = subprocess.run(cmd, capture_output=True, text=True)
    assert result.returncode == 1
    data = json.loads(result.stdout)
    assert data["status"] == "FAIL"
    assert "error" in data
