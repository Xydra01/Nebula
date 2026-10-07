"""Parse ``cargo nextest run`` output into one diagnostic per failing test.

nextest marks failures with ``FAIL [   0.0s] <binary> <test::path>`` lines, and the captured test
output carries a panic location ``thread '...' panicked at <file>:<line>:<col>``. We pair each
failing test with the nearest panic location; when none is captured we fall back to line 1 and note
that the location could not be recovered. The pass/fail outcome itself is the caller's job (from the
exit status) — this only enriches failures with locations.
"""

from __future__ import annotations

import re
from pathlib import PurePath

from ..model import Diagnostic, Severity

# "        FAIL [   0.012s] nebula-foo tests::math::adds_wrong"
# nextest may also print a progress counter after the time bracket: "FAIL [ 0.0s] (1/1) <bin>
# <test>". The run line is "FAIL [<time>] [(<n>/<m>)] <binary> <test-path>"; the binary name and
# the test path are the final two whitespace tokens, so the test path is the last token and the
# binary the one before it (both may contain "::" / "-" but no spaces).
_FAIL_LINE = re.compile(r"^\s*FAIL\s+\[[^\]]*\]\s+(?:\(\d+/\d+\)\s+)?\S+\s+(?P<test>\S+)\s*$")
# "thread 'tests::math::adds_wrong' panicked at crates/foo/src/lib.rs:42:9:"
# Newer Rust prints the thread id too: "thread 'tests::adds_wrong' (23556) panicked at ...", so the
# optional "(<id>)" segment between the thread name and "panicked at" is tolerated.
_PANIC_AT = re.compile(
    r"thread\s+'(?P<thread>[^']*)'\s+(?:\(\d+\)\s+)?panicked at\s+"
    r"(?P<file>[^:]+):(?P<line>\d+):(?P<col>\d+)"
)


def _relative(file: str, worktree_root: str) -> str:
    try:
        return str(PurePath(file).relative_to(PurePath(worktree_root)))
    except ValueError:
        return file


def parse_nextest(output: str, worktree_root: str) -> list[Diagnostic]:
    """Parse nextest output into one diagnostic per failing test.

    Returns an empty list when there are no ``FAIL`` lines (an all-pass run, or an output whose
    failure the caller derives from the exit status).
    """
    # Collect panic locations keyed by the thread/test name they mention.
    panic_by_test: dict[str, tuple[str, int, int]] = {}
    for line in output.splitlines():
        m = _PANIC_AT.search(line)
        if m:
            # The panicking thread name is the test path for a nextest test thread.
            panic_by_test[m.group("thread")] = (
                m.group("file"),
                int(m.group("line")),
                int(m.group("col")),
            )

    diagnostics: list[Diagnostic] = []
    seen: set[str] = set()
    for line in output.splitlines():
        fail = _FAIL_LINE.match(line)
        if not fail:
            continue
        test = fail.group("test")
        # nextest lists a failing test both inline and in the final summary; count each once.
        if test in seen:
            continue
        seen.add(test)
        loc = panic_by_test.get(test)
        if loc is not None:
            file, line_no, col = loc
            diagnostics.append(
                Diagnostic(
                    file=_relative(file, worktree_root),
                    line=line_no,
                    column=col,
                    message=f"{test}: test failed",
                    source="nextest",
                    severity=Severity.ERROR,
                )
            )
        else:
            diagnostics.append(
                Diagnostic(
                    file=test,
                    line=1,
                    message=f"{test}: test failed (panic location not captured)",
                    source="nextest",
                    severity=Severity.ERROR,
                )
            )
    return diagnostics
