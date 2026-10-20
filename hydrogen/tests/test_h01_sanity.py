import json
import re
import subprocess
from pathlib import Path


def test_hydrogen_sm89_sanity_check():
    """Verify Project Hydrogen SM 8.9 build harness, CUDA compilation, and GPU execution."""
    root_dir = Path(__file__).resolve().parent.parent
    build_script = root_dir / "scripts" / "build.ps1"
    assert build_script.is_file(), f"Build script not found: {build_script}"

    cmd = [
        "powershell",
        "-ExecutionPolicy",
        "Bypass",
        "-NoProfile",
        "-File",
        str(build_script),
        "-Run",
    ]

    result = subprocess.run(cmd, capture_output=True, text=True, cwd=str(root_dir.parent))
    assert result.returncode == 0, (
        f"build.ps1 failed with exit code {result.returncode}:\n{result.stderr}\n{result.stdout}"
    )

    # Extract JSON telemetry block from output
    match = re.search(r"\{[\s\S]*\}", result.stdout)
    assert match, f"No JSON telemetry block found in stdout:\n{result.stdout}"

    telemetry = json.loads(match.group(0))

    # Assert hardware discovery and execution invariants
    assert telemetry["status"] == "PASS", f"Sanity check status is not PASS: {telemetry}"
    assert telemetry["compute_capability"] == "8.9", (
        f"Unexpected compute capability: {telemetry['compute_capability']}"
    )
    assert "RTX 4070" in telemetry["device_name"], (
        f"Unexpected device name: {telemetry['device_name']}"
    )
    assert telemetry["multiprocessors"] == 46, (
        f"Unexpected SM count: {telemetry['multiprocessors']}"
    )
    assert telemetry["verified_elements"] == 1048576, (
        f"Verification element mismatch: {telemetry['verified_elements']}"
    )
    assert telemetry["vram_leak_bytes"] == 0, (
        f"VRAM memory leak detected: {telemetry['vram_leak_bytes']} bytes"
    )
    assert telemetry["kernel_duration_us"] < 5000.0, (
        f"Kernel execution exceeded 5ms WDDM budget: {telemetry['kernel_duration_us']} us"
    )
