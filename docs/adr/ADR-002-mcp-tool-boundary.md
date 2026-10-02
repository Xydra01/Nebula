# ADR-002: MCP as the universal tool boundary

**Status:** Accepted (2026-09-30)

## Context

Nebula needs built-in tools (filesystem, shell, git), Python tools it writes for itself, and third-party tools, including the cloud escalation tool. Giving each kind its own integration path would multiply the code and the attack surface.

## Decision

Every tool, built-in or generated, is exposed through MCP. Built-in Rust tools (filesystem, shell, git) implement the same interface in-process. External and Python tools run as MCP servers over stdio.

## Consequences

- One protocol for everything, and one place to enforce permission tiers and logging.
- Tools written for Nebula also work with other MCP clients, Cursor included.
- Third-party MCP servers, including the cloud escalation tool, plug in without special cases.
