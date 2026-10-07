"""Tests for the MCP protocol loop (issue #33, Requirement 1).

The server is driven over in-memory string streams: input lines in, response lines out. No child
processes are spawned (the registered tools here are stubs), so these run anywhere.
"""

from __future__ import annotations

import io
import json

from nebula_devtools.mcp import METHOD_NOT_FOUND, PROTOCOL_VERSION, Server, text_blocks


def _run(server: Server, requests: list[dict]) -> list[dict]:
    """Feed JSON-RPC requests through the server and return the parsed response objects."""
    stdin = io.StringIO("".join(json.dumps(r) + "\n" for r in requests))
    stdout = io.StringIO()
    server.serve(stdin=stdin, stdout=stdout)
    out = stdout.getvalue()
    # Every non-empty stdout line must be a JSON object (Requirement 1.4 / Property 4).
    responses = []
    for line in out.splitlines():
        if line.strip():
            responses.append(json.loads(line))  # raises if a non-JSON line leaked to stdout
    return responses


def _echo_server() -> Server:
    server = Server()
    server.register(
        "test.run",
        "stub",
        {"type": "object"},
        lambda args: (text_blocks(["ok"]), False),
    )
    return server


def test_initialize_advertises_protocol_version():
    responses = _run(
        _echo_server(), [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}]
    )
    assert len(responses) == 1
    result = responses[0]["result"]
    assert result["protocolVersion"] == PROTOCOL_VERSION
    assert "tools" in result["capabilities"]
    assert result["serverInfo"]["name"] == "nebula-devtools"


def test_initialized_notification_gets_no_reply():
    # A notification (no id) must not produce a response line.
    responses = _run(
        _echo_server(),
        [{"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}],
    )
    assert responses == []


def test_tools_list_returns_registered_tools():
    responses = _run(
        _echo_server(), [{"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}]
    )
    tools = responses[0]["result"]["tools"]
    names = [t["name"] for t in tools]
    assert names == ["test.run"]
    assert tools[0]["inputSchema"] == {"type": "object"}


def test_tools_call_returns_content_and_is_error():
    responses = _run(
        _echo_server(),
        [
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "test.run", "arguments": {}},
            }
        ],
    )
    result = responses[0]["result"]
    assert result["isError"] is False
    assert result["content"][0]["text"] == "ok"
    assert responses[0]["id"] == 3


def test_unknown_method_is_json_rpc_error():
    responses = _run(_echo_server(), [{"jsonrpc": "2.0", "id": 4, "method": "nope", "params": {}}])
    assert responses[0]["error"]["code"] == METHOD_NOT_FOUND


def test_unknown_tool_is_is_error_result_not_protocol_error():
    responses = _run(
        _echo_server(),
        [
            {
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "ghost.run", "arguments": {}},
            }
        ],
    )
    # Unknown tool is a tool-level failure (isError), not a JSON-RPC error.
    assert "error" not in responses[0]
    assert responses[0]["result"]["isError"] is True


def test_handler_exception_becomes_is_error_not_a_crash():
    server = Server()

    def boom(_args: dict):
        raise RuntimeError("kaboom")

    server.register("test.run", "stub", {"type": "object"}, boom)
    responses = _run(
        server,
        [
            {
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": {"name": "test.run", "arguments": {}},
            }
        ],
    )
    assert responses[0]["result"]["isError"] is True
    assert "kaboom" in responses[0]["result"]["content"][0]["text"]


def test_full_handshake_sequence_stdout_is_pure_json():
    # initialize -> initialized (no reply) -> tools/list -> tools/call. _run already asserts every
    # emitted line parses as JSON; here we check the response count and ids line up.
    responses = _run(
        _echo_server(),
        [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
            {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "test.run", "arguments": {}},
            },
        ],
    )
    assert [r["id"] for r in responses] == [1, 2, 3]  # the notification produced no response
