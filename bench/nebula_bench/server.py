"""Start and stop llama-server for a bench profile, and wait until it is healthy."""

from __future__ import annotations

import os
import secrets
import socket
import subprocess
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

import httpx

RUNTIMES = {
    "llama-prism": Path(r"F:\Nebula\runtime\llama-prism\b10743-adfffbe\llama-server.exe"),
}
LOG_DIR = Path(r"F:\Nebula\logs\bench")
PROFILE_DIR = Path(__file__).resolve().parent.parent / "profiles"


@dataclass
class Profile:
    name: str
    runtime: str
    model: str
    ctx: int
    kv_type: str
    flags: list[str]
    sampling: dict = field(default_factory=dict)

    @classmethod
    def load(cls, name: str) -> Profile:
        data = tomllib.loads((PROFILE_DIR / f"{name}.toml").read_text(encoding="utf-8"))
        return cls(**data)

    def args(self, port: int) -> list[str]:
        return [
            str(RUNTIMES[self.runtime]),
            "-m",
            self.model,
            "-c",
            str(self.ctx),
            "-ctk",
            self.kv_type,
            "-ctv",
            self.kv_type,
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            *self.flags,
        ]


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Server:
    """A llama-server child process. Use as a context manager so it is always stopped."""

    def __init__(self, profile: Profile, startup_timeout_s: float = 300.0):
        self.profile = profile
        self.port = free_port()
        self.base_url = f"http://127.0.0.1:{self.port}"
        self.startup_timeout_s = startup_timeout_s
        LOG_DIR.mkdir(parents=True, exist_ok=True)
        stamp = time.strftime("%Y%m%d-%H%M%S")
        self.log_path = LOG_DIR / f"{profile.name}-{stamp}.log"
        # Passed by environment rather than argv so it doesn't show up in process listings.
        self.api_key = secrets.token_urlsafe(32)
        self.headers = {"Authorization": f"Bearer {self.api_key}"}
        self._proc: subprocess.Popen | None = None
        self._log = None

    def __enter__(self) -> Server:
        self._log = self.log_path.open("wb")
        self._proc = subprocess.Popen(
            self.profile.args(self.port),
            stdout=self._log,
            stderr=subprocess.STDOUT,
            env={**os.environ, "LLAMA_API_KEY": self.api_key},
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        self.load_seconds = self._wait_healthy()
        return self

    def __exit__(self, *exc) -> None:
        if self._proc and self._proc.poll() is None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self._proc.kill()
        if self._log:
            self._log.close()

    def _wait_healthy(self) -> float:
        start = time.monotonic()
        while time.monotonic() - start < self.startup_timeout_s:
            if self._proc.poll() is not None:
                raise RuntimeError(
                    f"llama-server exited with code {self._proc.returncode}; see {self.log_path}"
                )
            try:
                if httpx.get(f"{self.base_url}/health", timeout=2).status_code == 200:
                    return time.monotonic() - start
            except httpx.HTTPError:
                pass
            time.sleep(1)
        raise TimeoutError(f"llama-server not healthy after {self.startup_timeout_s}s")

    def log_text(self) -> str:
        self._log.flush()
        return self.log_path.read_text(encoding="utf-8", errors="replace")
