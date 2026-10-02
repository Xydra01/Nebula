"""Python coding tasks for B5. Each has a prompt, hidden tests, and a reference solution."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Task:
    id: str
    lang: str
    kind: str
    prompt: str
    test: str
    reference: str


PY = []

PY.append(
    Task(
        "py_merge_intervals",
        "python",
        "algorithm",
        "Write a Python function `merge_intervals(intervals: list[tuple[int, int]]) -> "
        "list[tuple[int, int]]` that merges overlapping or touching closed intervals (for example "
        "(1, 3) and (3, 5) merge into (1, 5)). The input may be unsorted and may contain intervals "
        "given as (start, end) with start <= end. Return the merged intervals sorted by start.",
        """
assert merge_intervals([]) == []
assert merge_intervals([(1, 3), (2, 6), (8, 10), (15, 18)]) == [(1, 6), (8, 10), (15, 18)]
assert merge_intervals([(1, 4), (4, 5)]) == [(1, 5)]
assert merge_intervals([(5, 7), (1, 2)]) == [(1, 2), (5, 7)]
assert merge_intervals([(1, 10), (2, 3), (4, 5)]) == [(1, 10)]
assert merge_intervals([(3, 3)]) == [(3, 3)]
assert merge_intervals([(6, 8), (1, 9), (2, 4), (4, 7)]) == [(1, 9)]
""",
        """
def merge_intervals(intervals):
    out = []
    for s, e in sorted(intervals):
        if out and s <= out[-1][1]:
            out[-1] = (out[-1][0], max(out[-1][1], e))
        else:
            out.append((s, e))
    return out
""",
    )
)

PY.append(
    Task(
        "py_lru_cache",
        "python",
        "algorithm",
        "Implement a Python class `LRUCache` with `__init__(self, capacity: int)`, "
        "`get(self, key) -> object` (returns -1 if missing) and `put(self, key, value) -> None`. "
        "When inserting beyond capacity, evict the least recently used key. Both `get` and `put` "
        "count as a use. Both operations must be O(1) on average. Do not use functools.",
        """
c = LRUCache(2)
c.put(1, 1); c.put(2, 2)
assert c.get(1) == 1
c.put(3, 3)
assert c.get(2) == -1
c.put(4, 4)
assert c.get(1) == -1
assert c.get(3) == 3 and c.get(4) == 4
c = LRUCache(1)
c.put("a", 1); c.put("a", 2)
assert c.get("a") == 2
c.put("b", 3)
assert c.get("a") == -1 and c.get("b") == 3
c = LRUCache(2)
c.put(1, 1); c.put(2, 2); c.put(1, 10)
c.put(3, 3)
assert c.get(2) == -1 and c.get(1) == 10
""",
        """
from collections import OrderedDict
class LRUCache:
    def __init__(self, capacity):
        self.cap = capacity; self.d = OrderedDict()
    def get(self, key):
        if key not in self.d: return -1
        self.d.move_to_end(key); return self.d[key]
    def put(self, key, value):
        self.d[key] = value; self.d.move_to_end(key)
        if len(self.d) > self.cap: self.d.popitem(last=False)
""",
    )
)

PY.append(
    Task(
        "py_fix_bisect",
        "python",
        "bugfix",
        "This function should return the leftmost index at which `x` could be inserted into the "
        "sorted list `a` while keeping it sorted (like `bisect.bisect_left`), but it has bugs. "
        "Fix it without using the bisect module, and keep it O(log n).\n\n"
        "```python\ndef insertion_point(a, x):\n    lo, hi = 0, len(a) - 1\n    while lo < hi:\n"
        "        mid = (lo + hi) // 2\n        if a[mid] <= x:\n            lo = mid\n        else:\n"
        "            hi = mid - 1\n    return lo\n```",
        """
import bisect, random
random.seed(1)
for _ in range(2000):
    a = sorted(random.randint(0, 20) for _ in range(random.randint(0, 15)))
    x = random.randint(-2, 22)
    assert insertion_point(a, x) == bisect.bisect_left(a, x), (a, x)
assert insertion_point([], 5) == 0
""",
        """
def insertion_point(a, x):
    lo, hi = 0, len(a)
    while lo < hi:
        mid = (lo + hi) // 2
        if a[mid] < x: lo = mid + 1
        else: hi = mid
    return lo
""",
    )
)

PY.append(
    Task(
        "py_parse_duration",
        "python",
        "algorithm",
        "Write `parse_duration(s: str) -> int` that converts strings like '1h30m', '45s', "
        "'2d4h', '1h 15m 10s' into total seconds. Units: d, h, m, s. Units must appear in that "
        "order, each at most once, with a positive integer before each. Whitespace between parts "
        "is allowed. Raise ValueError for an empty string, unknown units, wrong order, repeated "
        "units, or missing numbers.",
        """
assert parse_duration("45s") == 45
assert parse_duration("1h30m") == 5400
assert parse_duration("2d4h") == 2 * 86400 + 4 * 3600
assert parse_duration("1h 15m 10s") == 3600 + 900 + 10
assert parse_duration("0m") == 0
for bad in ["", "10", "5x", "1m1h", "1h1h", "h", "1h m", "-5s", "1.5h"]:
    try:
        parse_duration(bad)
    except ValueError:
        pass
    else:
        raise AssertionError(bad)
""",
        """
import re
def parse_duration(s):
    m = re.fullmatch(r"\\s*(?:(\\d+)d)?\\s*(?:(\\d+)h)?\\s*(?:(\\d+)m)?\\s*(?:(\\d+)s)?\\s*", s)
    if not s.strip() or not m or not any(m.groups()): raise ValueError(s)
    d, h, mi, se = (int(g) if g else 0 for g in m.groups())
    return d * 86400 + h * 3600 + mi * 60 + se
""",
    )
)

PY.append(
    Task(
        "py_topo_sort",
        "python",
        "algorithm",
        "Write `topo_sort(deps: dict[str, list[str]]) -> list[str]` where `deps[x]` lists the "
        "items x depends on. Return an order in which every item appears after all of its "
        "dependencies. Items that appear only as dependencies must be included too. When several "
        "items are available at once, take them in alphabetical order (so the result is "
        "deterministic). Raise ValueError if there is a cycle.",
        """
assert topo_sort({}) == []
assert topo_sort({"b": ["a"], "c": ["b"]}) == ["a", "b", "c"]
assert topo_sort({"app": ["log", "db"], "db": ["log"]}) == ["log", "db", "app"]
assert topo_sort({"x": [], "a": [], "m": ["a"]}) == ["a", "m", "x"]
assert topo_sort({"d": ["b", "c"], "b": ["a"], "c": ["a"]}) == ["a", "b", "c", "d"]
for cyc in [{"a": ["b"], "b": ["a"]}, {"a": ["a"]}, {"a": ["b"], "b": ["c"], "c": ["a"]}]:
    try:
        topo_sort(cyc)
    except ValueError:
        pass
    else:
        raise AssertionError(cyc)
""",
        """
import heapq
def topo_sort(deps):
    nodes = set(deps) | {d for ds in deps.values() for d in ds}
    indeg = {n: 0 for n in nodes}; users = {n: [] for n in nodes}
    for n, ds in deps.items():
        for d in set(ds):
            indeg[n] += 1; users[d].append(n)
    ready = [n for n in nodes if indeg[n] == 0]; heapq.heapify(ready); out = []
    while ready:
        n = heapq.heappop(ready); out.append(n)
        for u in users[n]:
            indeg[u] -= 1
            if indeg[u] == 0: heapq.heappush(ready, u)
    if len(out) != len(nodes): raise ValueError("cycle")
    return out
""",
    )
)

PY.append(
    Task(
        "py_refactor_invoice",
        "python",
        "refactor",
        "Refactor this function into clean, readable code with the same name and exactly the same "
        "behavior for every input (including the rounding). You may add helper functions.\n\n"
        "```python\ndef invoice_total(items, coupon=None):\n    t = 0\n    for i in items:\n"
        "        if i['qty'] > 0:\n            p = i['price'] * i['qty']\n"
        "            if i.get('category') == 'food':\n                p = p * 1.05\n            else:\n"
        "                p = p * 1.13\n            t = t + p\n    if coupon != None:\n"
        "        if coupon['type'] == 'percent':\n            t = t - t * coupon['value'] / 100\n"
        "        elif coupon['type'] == 'fixed':\n            t = t - coupon['value']\n"
        "            if t < 0:\n                t = 0\n    return round(t, 2)\n```",
        """
items = [{"price": 10, "qty": 2, "category": "food"}, {"price": 5, "qty": 1},
         {"price": 3, "qty": 0, "category": "food"}, {"price": 7, "qty": -1}]
assert invoice_total([]) == 0
assert invoice_total(items) == round(21 + 5.65, 2)
assert invoice_total(items, {"type": "percent", "value": 10}) == round((21 + 5.65) * 0.9, 2)
assert invoice_total(items, {"type": "fixed", "value": 100}) == 0
assert invoice_total(items, {"type": "fixed", "value": 5}) == round(26.65 - 5, 2)
assert invoice_total(items, {"type": "bogus", "value": 5}) == 26.65
assert invoice_total([{"price": 0.1, "qty": 3, "category": "toys"}]) == round(0.1 * 3 * 1.13, 2)
""",
        """
def invoice_total(items, coupon=None):
    t = 0
    for i in items:
        if i['qty'] > 0:
            p = i['price'] * i['qty']
            p = p * 1.05 if i.get('category') == 'food' else p * 1.13
            t = t + p
    if coupon is not None:
        if coupon['type'] == 'percent':
            t = t - t * coupon['value'] / 100
        elif coupon['type'] == 'fixed':
            t = max(t - coupon['value'], 0)
    return round(t, 2)
""",
    )
)

PY.append(
    Task(
        "py_expr_eval",
        "python",
        "algorithm",
        "Write `evaluate(expr: str) -> float` that evaluates arithmetic expressions with +, -, *, /, "
        "parentheses, unary minus and decimal numbers, with the usual precedence and left "
        "associativity. Whitespace may appear anywhere. Do not use eval, exec, compile or the ast "
        "module. Raise ValueError for malformed input, and ZeroDivisionError for division by zero.",
        """
import math
cases = {"1 + 2 * 3": 7, "(1 + 2) * 3": 9, "10 / 4": 2.5, "2 - 3 - 4": -5, "8 / 2 / 2": 2,
         "-3 + 5": 2, "-(2 + 3) * 2": -10, " 3.5 * 2 ": 7, "2 * -3": -6, "((4))": 4,
         "1 - -1": 2}
for e, v in cases.items():
    assert math.isclose(evaluate(e), v), (e, evaluate(e))
for bad in ["", "1 +", "(1 + 2", "1 2", "1 + * 2", ")"]:
    try:
        evaluate(bad)
    except ValueError:
        pass
    else:
        raise AssertionError(bad)
try:
    evaluate("1 / (2 - 2)")
except ZeroDivisionError:
    pass
else:
    raise AssertionError("div0")
""",
        """
import re
def evaluate(expr):
    toks = re.findall(r"\\d+\\.?\\d*|\\.\\d+|[-+*/()]|\\S", expr)
    pos = 0
    def peek(): return toks[pos] if pos < len(toks) else None
    def take():
        nonlocal pos; t = peek(); pos += 1; return t
    def factor():
        t = take()
        if t is None: raise ValueError("eof")
        if t == "-": return -factor()
        if t == "(":
            v = expr_(); 
            if take() != ")": raise ValueError(")")
            return v
        try: return float(t)
        except ValueError: raise ValueError(t)
    def term():
        v = factor()
        while peek() in ("*", "/"):
            op = take(); r = factor()
            v = v * r if op == "*" else v / r
        return v
    def expr_():
        v = term()
        while peek() in ("+", "-"):
            op = take(); r = term()
            v = v + r if op == "+" else v - r
        return v
    v = expr_()
    if pos != len(toks): raise ValueError("trailing")
    return v
""",
    )
)

PY.append(
    Task(
        "py_token_bucket",
        "python",
        "algorithm",
        "Implement `class TokenBucket` with `__init__(self, rate: float, capacity: int, clock)` and "
        "`allow(self, cost: int = 1) -> bool`. `clock` is a zero-argument callable returning the "
        "current time in seconds (float). The bucket starts full; tokens refill continuously at "
        "`rate` per second up to `capacity`. `allow` consumes `cost` tokens and returns True if "
        "enough are available, otherwise it consumes nothing and returns False.",
        """
class Clock:
    def __init__(self): self.t = 0.0
    def __call__(self): return self.t
c = Clock()
b = TokenBucket(rate=2, capacity=4, clock=c)
assert all(b.allow() for _ in range(4))
assert not b.allow()
c.t = 0.5
assert b.allow() and not b.allow()
c.t = 10
assert b.allow(4) and not b.allow(1)
c.t = 11
assert b.allow(2) and not b.allow(1)
assert not b.allow(5)
c.t = 13
assert b.allow(3)
""",
        """
class TokenBucket:
    def __init__(self, rate, capacity, clock):
        self.rate, self.cap, self.clock = rate, capacity, clock
        self.tokens = float(capacity); self.last = clock()
    def allow(self, cost=1):
        now = self.clock()
        self.tokens = min(self.cap, self.tokens + (now - self.last) * self.rate); self.last = now
        if self.tokens >= cost:
            self.tokens -= cost; return True
        return False
""",
    )
)

PY.append(
    Task(
        "py_fix_dedupe",
        "python",
        "bugfix",
        "This function should return the strings with duplicates removed, comparing "
        "case-insensitively and keeping the first occurrence (with its original casing) in the "
        "original order. Each call must be independent of previous calls. It has bugs; fix them.\n\n"
        "```python\ndef dedupe(words, seen=set()):\n    out = []\n    for w in words:\n"
        "        if w not in seen:\n            seen.add(w)\n            out.append(w.lower())\n"
        "    return sorted(out)\n```",
        """
assert dedupe(["b", "A", "a", "B", "c"]) == ["b", "A", "c"]
assert dedupe(["b", "A"]) == ["b", "A"]
assert dedupe([]) == []
assert dedupe(["Go", "GO", "go", "Rust"]) == ["Go", "Rust"]
""",
        """
def dedupe(words):
    seen, out = set(), []
    for w in words:
        k = w.lower()
        if k not in seen:
            seen.add(k); out.append(w)
    return out
""",
    )
)

PY.append(
    Task(
        "py_get_path",
        "python",
        "algorithm",
        "Write `get_path(obj, path: str, default=None)` that reads nested data from dicts and lists "
        "using paths like 'a.b[2].c', 'users[0].name' or '[1][0]'. Keys are separated by dots; list "
        "indexes are written in brackets and may be negative. Return `default` if any step is "
        "missing, out of range, or applied to the wrong type. An empty path returns obj.",
        """
data = {"a": {"b": [1, 2, {"c": "x"}]}, "users": [{"name": "ann"}, {"name": "bob"}], "n": None}
assert get_path(data, "a.b[2].c") == "x"
assert get_path(data, "users[1].name") == "bob"
assert get_path(data, "users[-1].name") == "bob"
assert get_path(data, "users[5].name", "?") == "?"
assert get_path(data, "a.z", 0) == 0
assert get_path(data, "a.b.c", "bad") == "bad"
assert get_path(data, "n") is None
assert get_path(data, "n.x", 7) == 7
assert get_path([[1, 2], [3]], "[1][0]") == 3
assert get_path(data, "") is data
""",
        """
import re
def get_path(obj, path, default=None):
    if path == "": return obj
    cur = obj
    for key, idx in re.findall(r"([^.\\[\\]]+)|\\[(-?\\d+)\\]", path):
        if key:
            if not isinstance(cur, dict) or key not in cur: return default
            cur = cur[key]
        else:
            i = int(idx)
            if not isinstance(cur, list) or not -len(cur) <= i < len(cur): return default
            cur = cur[i]
    return cur
""",
    )
)

PY.append(
    Task(
        "py_gather_limited",
        "python",
        "algorithm",
        "Write `async def gather_limited(factories, limit: int) -> list` where `factories` is a list "
        "of zero-argument callables that each return a coroutine. Run them with at most `limit` "
        "running at the same time, and return their results in the same order as `factories`. If "
        "any coroutine raises, propagate the exception.",
        """
import asyncio
running = peak = 0
async def job(i, d):
    global running, peak
    running += 1; peak = max(peak, running)
    await asyncio.sleep(d)
    running -= 1
    return i * 10
fs = [lambda i=i: job(i, 0.01 * (5 - i % 5)) for i in range(12)]
res = asyncio.run(gather_limited(fs, 3))
assert res == [i * 10 for i in range(12)], res
assert peak <= 3 and peak >= 2, peak
async def boom():
    raise KeyError("x")
try:
    asyncio.run(gather_limited([boom], 2))
except KeyError:
    pass
else:
    raise AssertionError("no raise")
assert asyncio.run(gather_limited([], 4)) == []
""",
        """
import asyncio
async def gather_limited(factories, limit):
    sem = asyncio.Semaphore(limit)
    async def run(f):
        async with sem: return await f()
    return list(await asyncio.gather(*(run(f) for f in factories)))
""",
    )
)

PY.append(
    Task(
        "py_lcs",
        "python",
        "algorithm",
        "Write `lcs_diff(a: list[str], b: list[str]) -> list[str]` that produces a line diff "
        "turning `a` into `b`, based on a longest common subsequence. Output lines are ' x' for "
        "kept lines, '-x' for deleted lines and '+x' for added lines. Applying the diff must "
        "reproduce `b`, and the number of kept lines must equal the LCS length.",
        """
def apply(a, d):
    out, i = [], 0
    for line in d:
        tag, text = line[0], line[1:]
        if tag == " ": assert a[i] == text; out.append(text); i += 1
        elif tag == "-": assert a[i] == text; i += 1
        elif tag == "+": out.append(text)
        else: raise AssertionError(line)
    assert i == len(a)
    return out
def lcs_len(a, b):
    m = [[0] * (len(b) + 1) for _ in range(len(a) + 1)]
    for i in range(len(a)):
        for j in range(len(b)):
            m[i+1][j+1] = m[i][j] + 1 if a[i] == b[j] else max(m[i][j+1], m[i+1][j])
    return m[-1][-1]
import random
random.seed(3)
cases = [([], []), (["a"], []), ([], ["b"]), (list("abcabba"), list("cbabac"))]
for _ in range(200):
    cases.append(([random.choice("xyz") for _ in range(random.randint(0, 8))],
                  [random.choice("xyz") for _ in range(random.randint(0, 8))]))
for a, b in cases:
    d = lcs_diff(a, b)
    assert apply(a, d) == b, (a, b, d)
    assert sum(1 for x in d if x[0] == " ") == lcs_len(a, b), (a, b, d)
""",
        """
def lcs_diff(a, b):
    n, m = len(a), len(b)
    L = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n - 1, -1, -1):
        for j in range(m - 1, -1, -1):
            L[i][j] = L[i+1][j+1] + 1 if a[i] == b[j] else max(L[i+1][j], L[i][j+1])
    out, i, j = [], 0, 0
    while i < n and j < m:
        if a[i] == b[j]: out.append(" " + a[i]); i += 1; j += 1
        elif L[i+1][j] >= L[i][j+1]: out.append("-" + a[i]); i += 1
        else: out.append("+" + b[j]); j += 1
    out += ["-" + x for x in a[i:]] + ["+" + x for x in b[j:]]
    return out
""",
    )
)
