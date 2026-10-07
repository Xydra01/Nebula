"""Parsers that turn captured runner output into structured diagnostics.

Each parser maps a runner's output to a list of :class:`~nebula_devtools.model.Diagnostic`. Where a
runner offers machine-readable output (cargo/clippy/ruff JSON), the parser consumes that rather than
scraping human text, for robustness. The overall pass/fail outcome is decided by the caller from the
runner's exit status; parsers only enrich a failed run with locations and never assert success.
"""
