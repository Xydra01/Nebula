"""Entry point: run the nebula-devtools MCP server over stdin/stdout.

Logging is configured to **stderr** so stdout stays a clean JSON-RPC protocol channel
(issue #33, Requirement 1.4). The server loop handles each request without raising, so an
unexpected error is logged to stderr rather than crashing mid-request or corrupting stdout.
"""

from __future__ import annotations

import logging
import sys

from .server import build_server


def main() -> int:
    """Configure stderr logging and serve the MCP loop until stdin closes."""
    logging.basicConfig(
        stream=sys.stderr,
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    server = build_server()
    try:
        server.serve()
    except KeyboardInterrupt:
        return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
