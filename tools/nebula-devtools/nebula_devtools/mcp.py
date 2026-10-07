"""A minimal MCP server loop: newline-delimited JSON-RPC 2.0 over stdin/stdout.

Implements exactly what Nebula's tool host drives (``crates/nebula-tools/src/client.rs``):
``initialize`` -> ``notifications/initialized``, ``tools/list``, and ``tools/call``, protocol
version ``2025-06-18``. One JSON object per line. **stdout is the protocol channel** — all logging
goes to stderr (issue #33, Requirement 1).
"""

from __future__ import annotations

import json
import logging
import sys
from collections.abc import Callable
from typing import TextIO

PROTOCOL_VERSION = "2025-06-18"
SERVER_NAME = "nebula-devtools"

#: JSON-RPC error code for an unknown method (per the JSON-RPC 2.0 spec).
METHOD_NOT_FOUND = -32601
PARSE_ERROR = -32700
INVALID_REQUEST = -32600

logger = logging.getLogger(SERVER_NAME)

#: A tool handler takes the call's ``arguments`` dict and returns an MCP ``(content, is_error)``.
ToolHandler = Callable[[dict], tuple[list[dict], bool]]


class Server:
    """The MCP server: a tool registry plus the stdin/stdout request loop.

    Register tools with :meth:`register`, then call :meth:`serve` to run the loop until stdin
    closes. The loop never writes non-JSON to stdout and never raises out of a single request —
    a handler error becomes an ``isError`` tool result, and a protocol error becomes a JSON-RPC
    error response (Requirement 1.4, 1.5, 8.3).
    """

    def __init__(self) -> None:
        self._tools: dict[str, dict] = {}
        self._handlers: dict[str, ToolHandler] = {}

    def register(
        self, name: str, description: str, input_schema: dict, handler: ToolHandler
    ) -> None:
        """Register a tool: its descriptor (for ``tools/list``) and its call handler."""
        self._tools[name] = {
            "name": name,
            "description": description,
            "inputSchema": input_schema,
        }
        self._handlers[name] = handler

    def serve(self, stdin: TextIO | None = None, stdout: TextIO | None = None) -> None:
        """Run the request loop until the input stream closes."""
        stdin = stdin or sys.stdin
        stdout = stdout or sys.stdout
        for raw in stdin:
            line = raw.strip()
            if not line:
                continue
            response = self._handle_line(line)
            if response is not None:
                self._write(stdout, response)

    def _write(self, stdout: TextIO, message: dict) -> None:
        """Write one JSON-RPC object as a single line, then flush (stdout is protocol-only)."""
        stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
        stdout.flush()

    def _handle_line(self, line: str) -> dict | None:
        """Dispatch one input line; return a response dict, or ``None`` for a notification."""
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            logger.warning("ignoring non-JSON input line")
            return _error(None, PARSE_ERROR, "parse error")
        if not isinstance(message, dict):
            return _error(None, INVALID_REQUEST, "request must be a JSON object")

        method = message.get("method")
        msg_id = message.get("id")

        # A message with no id is a notification: act on it but never reply (Requirement 1.1).
        if msg_id is None:
            if method == "notifications/initialized":
                logger.info("client initialized")
            return None

        if method == "initialize":
            return _result(
                msg_id,
                {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": SERVER_NAME, "version": _version()},
                },
            )
        if method == "tools/list":
            return _result(msg_id, {"tools": list(self._tools.values())})
        if method == "tools/call":
            return self._handle_call(msg_id, message.get("params") or {})
        return _error(msg_id, METHOD_NOT_FOUND, f"unknown method {method!r}")

    def _handle_call(self, msg_id: object, params: dict) -> dict:
        """Run a ``tools/call``; an unknown tool or handler error becomes an ``isError`` result."""
        name = params.get("name")
        arguments = params.get("arguments") or {}
        handler = self._handlers.get(name) if isinstance(name, str) else None
        if handler is None:
            return _result(
                msg_id,
                {"content": _text_blocks([f"unknown tool {name!r}"]), "isError": True},
            )
        try:
            content, is_error = handler(arguments)
        except Exception as e:  # noqa: BLE001 - a handler must never crash the protocol loop
            logger.exception("tool %s raised", name)
            return _result(
                msg_id,
                {"content": _text_blocks([f"tool {name} failed: {e}"]), "isError": True},
            )
        return _result(msg_id, {"content": content, "isError": is_error})


def _text_blocks(texts: list[str]) -> list[dict]:
    """Wrap plain strings as MCP text content blocks."""
    return [{"type": "text", "text": t} for t in texts]


def _result(msg_id: object, result: dict) -> dict:
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def _error(msg_id: object, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": msg_id, "error": {"code": code, "message": message}}


def _version() -> str:
    from . import __version__

    return __version__


def text_blocks(texts: list[str]) -> list[dict]:
    """Public helper for building MCP text content blocks (used by the server registry)."""
    return _text_blocks(texts)
