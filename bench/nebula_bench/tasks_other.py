"""Rust and TypeScript coding tasks for B5."""

from __future__ import annotations

from nebula_bench.tasks_py import Task

RS: list[Task] = []
TS: list[Task] = []

RS.append(
    Task(
        "rs_roman",
        "rust",
        "algorithm",
        "Write a Rust function `pub fn roman_to_int(s: &str) -> Option<u32>` that parses a Roman "
        "numeral in standard form (I, V, X, L, C, D, M with subtractive pairs IV, IX, XL, XC, CD, "
        "CM) for values 1 to 3999. Return None for an empty string, invalid characters, or "
        'non-canonical forms such as "IIII", "VX", "IC" or "MMMM".',
        """
    assert_eq!(roman_to_int("III"), Some(3));
    assert_eq!(roman_to_int("LVIII"), Some(58));
    assert_eq!(roman_to_int("MCMXCIV"), Some(1994));
    assert_eq!(roman_to_int("MMMCMXCIX"), Some(3999));
    assert_eq!(roman_to_int("XLII"), Some(42));
    for bad in ["", "IIII", "VX", "IC", "MMMM", "ABC", "IIV", "VV", "XM", "iv"] {
        assert_eq!(roman_to_int(bad), None, "{bad}");
    }
""",
        """
pub fn roman_to_int(s: &str) -> Option<u32> {
    fn to_roman(mut n: u32) -> String {
        let t = [(1000,"M"),(900,"CM"),(500,"D"),(400,"CD"),(100,"C"),(90,"XC"),(50,"L"),
                 (40,"XL"),(10,"X"),(9,"IX"),(5,"V"),(4,"IV"),(1,"I")];
        let mut out = String::new();
        for (v, r) in t { while n >= v { out.push_str(r); n -= v; } }
        out
    }
    let val = |c| match c { 'I'=>Some(1),'V'=>Some(5),'X'=>Some(10),'L'=>Some(50),
                            'C'=>Some(100),'D'=>Some(500),'M'=>Some(1000),_=>None };
    if s.is_empty() { return None; }
    let v: Vec<u32> = s.chars().map(val).collect::<Option<_>>()?;
    let mut total = 0;
    for i in 0..v.len() {
        if i + 1 < v.len() && v[i] < v[i+1] { total -= v[i] as i64; } else { total += v[i] as i64; }
    }
    if total < 1 || total > 3999 { return None; }
    let n = total as u32;
    if to_roman(n) == s { Some(n) } else { None }
}
""",
    )
)

RS.append(
    Task(
        "rs_word_freq",
        "rust",
        "algorithm",
        "Write `pub fn word_freq(text: &str) -> Vec<(String, usize)>` in Rust. Words are maximal "
        "runs of ASCII alphanumeric characters, compared case-insensitively and reported in "
        "lowercase. Return (word, count) pairs sorted by count descending, then by word ascending.",
        """
    assert_eq!(word_freq(""), vec![]);
    assert_eq!(word_freq("b a B, a; A!"), vec![("a".to_string(), 3), ("b".to_string(), 2)]);
    assert_eq!(word_freq("x2 y x2-z"),
               vec![("x2".to_string(), 2), ("y".to_string(), 1), ("z".to_string(), 1)]);
    assert_eq!(word_freq("  ..  "), vec![]);
""",
        """
pub fn word_freq(text: &str) -> Vec<(String, usize)> {
    use std::collections::HashMap;
    let mut m: HashMap<String, usize> = HashMap::new();
    for w in text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
        *m.entry(w.to_ascii_lowercase()).or_default() += 1;
    }
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}
""",
    )
)

RS.append(
    Task(
        "rs_fix_brackets",
        "rust",
        "bugfix",
        "This Rust function should return true when every bracket in the string ((), [], {}) is "
        "properly matched and nested, ignoring all other characters. It has bugs. Fix it and "
        "return the complete corrected function.\n\n```rust\npub fn balanced(s: &str) -> bool {\n"
        "    let mut stack = Vec::new();\n    for c in s.chars() {\n        match c {\n"
        "            '(' | '[' | '{' => stack.push(c),\n            ')' | ']' | '}' => {\n"
        "                let open = stack.pop().unwrap();\n"
        "                if open != c { return false; }\n            }\n            _ => {}\n"
        "        }\n    }\n    true\n}\n```",
        """
    assert!(balanced(""));
    assert!(balanced("a(b[c]{d}e)f"));
    assert!(balanced("{[()()]}"));
    assert!(!balanced("("));
    assert!(!balanced(")"));
    assert!(!balanced("(]"));
    assert!(!balanced("([)]"));
    assert!(!balanced("(()"));
""",
        """
pub fn balanced(s: &str) -> bool {
    let mut stack = Vec::new();
    for c in s.chars() {
        match c {
            '(' | '[' | '{' => stack.push(c),
            ')' | ']' | '}' => {
                let want = match c { ')' => '(', ']' => '[', _ => '{' };
                if stack.pop() != Some(want) { return false; }
            }
            _ => {}
        }
    }
    stack.is_empty()
}
""",
    )
)

RS.append(
    Task(
        "rs_ring_buffer",
        "rust",
        "algorithm",
        "Implement a generic `pub struct RingBuffer<T>` in Rust with: `pub fn new(capacity: usize) "
        "-> Self` (capacity > 0), `pub fn push(&mut self, item: T)` which overwrites the oldest item "
        "when full, `pub fn len(&self) -> usize`, `pub fn capacity(&self) -> usize`, and `pub fn "
        "iter(&self) -> impl Iterator<Item = &T>` that yields items from oldest to newest. Use only "
        "the standard library.",
        """
    let mut r = RingBuffer::new(3);
    assert_eq!(r.len(), 0);
    assert_eq!(r.capacity(), 3);
    r.push(1); r.push(2);
    assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![1, 2]);
    r.push(3); r.push(4); r.push(5);
    assert_eq!(r.len(), 3);
    assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![3, 4, 5]);
    let mut s = RingBuffer::new(1);
    s.push("a".to_string()); s.push("b".to_string());
    assert_eq!(s.iter().cloned().collect::<Vec<_>>(), vec!["b".to_string()]);
""",
        """
pub struct RingBuffer<T> { buf: std::collections::VecDeque<T>, cap: usize }
impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        Self { buf: std::collections::VecDeque::with_capacity(capacity), cap: capacity }
    }
    pub fn push(&mut self, item: T) {
        if self.buf.len() == self.cap { self.buf.pop_front(); }
        self.buf.push_back(item);
    }
    pub fn len(&self) -> usize { self.buf.len() }
    pub fn capacity(&self) -> usize { self.cap }
    pub fn iter(&self) -> impl Iterator<Item = &T> { self.buf.iter() }
}
""",
    )
)

TS_NOTE = (
    " Use only erasable TypeScript syntax (no enums, namespaces or parameter properties), "
    "because the code runs with Node's type stripping."
)

TS.append(
    Task(
        "ts_group_by",
        "typescript",
        "algorithm",
        "Write a TypeScript function `groupBy<T, K>(items: T[], key: (item: T) => K): Map<K, T[]>` "
        "that groups items by key, preserving the order of first appearance of each key and the "
        "order of items within each group." + TS_NOTE,
        """
const m = groupBy([1, 2, 3, 4, 5, 6], (n) => n % 3);
assert.deepEqual([...m.keys()], [1, 2, 0]);
assert.deepEqual(m.get(0), [3, 6]);
assert.deepEqual(m.get(1), [1, 4]);
assert.equal(groupBy([], (x: number) => x).size, 0);
const w = groupBy(["apple", "avocado", "banana"], (s) => s[0]);
assert.deepEqual(w.get("a"), ["apple", "avocado"]);
""",
        """
function groupBy<T, K>(items: T[], key: (item: T) => K): Map<K, T[]> {
  const m = new Map<K, T[]>();
  for (const it of items) {
    const k = key(it);
    const g = m.get(k);
    if (g) g.push(it); else m.set(k, [it]);
  }
  return m;
}
""",
    )
)

TS.append(
    Task(
        "ts_semver",
        "typescript",
        "algorithm",
        "Write a TypeScript function `compareSemver(a: string, b: string): -1 | 0 | 1` following "
        "Semantic Versioning 2.0.0 precedence: compare MAJOR.MINOR.PATCH numerically; a version with "
        "a pre-release (after '-') is lower than the same version without one; pre-release "
        "identifiers are compared dot by dot, numeric identifiers numerically and lower than "
        "alphanumeric ones, and a shorter set of identifiers is lower if all preceding ones are "
        "equal. Build metadata (after '+') is ignored." + TS_NOTE,
        """
const order = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta",
  "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.2.0", "1.10.0", "2.0.0"];
for (let i = 0; i < order.length; i++) {
  for (let j = 0; j < order.length; j++) {
    assert.equal(compareSemver(order[i], order[j]), Math.sign(i - j), `${order[i]} ${order[j]}`);
  }
}
assert.equal(compareSemver("1.0.0+build.5", "1.0.0+other"), 0);
assert.equal(compareSemver("1.0.0-alpha+x", "1.0.0-alpha"), 0);
""",
        """
function compareSemver(a: string, b: string): -1 | 0 | 1 {
  const parse = (v: string) => {
    const [core, pre] = v.split("+")[0].split(/-(.*)/s);
    return { nums: core.split(".").map(Number), pre: pre ? pre.split(".") : [] };
  };
  const A = parse(a), B = parse(b);
  for (let i = 0; i < 3; i++) if (A.nums[i] !== B.nums[i]) return A.nums[i] < B.nums[i] ? -1 : 1;
  if (!A.pre.length || !B.pre.length) {
    if (A.pre.length === B.pre.length) return 0;
    return A.pre.length ? -1 : 1;
  }
  for (let i = 0; i < Math.max(A.pre.length, B.pre.length); i++) {
    const x = A.pre[i], y = B.pre[i];
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    const xn = /^\\d+$/.test(x), yn = /^\\d+$/.test(y);
    if (xn && yn) { if (+x !== +y) return +x < +y ? -1 : 1; }
    else if (xn !== yn) return xn ? -1 : 1;
    else if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}
""",
    )
)

TS.append(
    Task(
        "ts_fix_deep_equal",
        "typescript",
        "bugfix",
        "This TypeScript function should report whether two JSON-like values (null, booleans, "
        "numbers, strings, arrays, plain objects) are deeply equal. It has bugs. Fix it and return "
        "the complete corrected function." + TS_NOTE + "\n\n```typescript\n"
        "function deepEqual(a: unknown, b: unknown): boolean {\n  if (a === b) return true;\n"
        '  if (typeof a !== "object" || typeof b !== "object") return false;\n'
        "  const ka = Object.keys(a as object);\n  for (const k of ka) {\n"
        "    if (!deepEqual((a as any)[k], (b as any)[k])) return false;\n  }\n  return true;\n}\n```",
        """
assert.equal(deepEqual({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] }), true);
assert.equal(deepEqual({ a: 1 }, { a: 1, b: 2 }), false);
assert.equal(deepEqual({ a: 1, b: 2 }, { a: 1 }), false);
assert.equal(deepEqual([1, 2], { 0: 1, 1: 2 }), false);
assert.equal(deepEqual(null, {}), false);
assert.equal(deepEqual({}, null), false);
assert.equal(deepEqual(null, null), true);
assert.equal(deepEqual([1, 2], [1, 2, 3]), false);
assert.equal(deepEqual([], []), true);
assert.equal(deepEqual("1", 1), false);
""",
        """
function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a as object), kb = Object.keys(b as object);
  if (ka.length !== kb.length) return false;
  for (const k of ka) {
    if (!Object.prototype.hasOwnProperty.call(b, k)) return false;
    if (!deepEqual((a as any)[k], (b as any)[k])) return false;
  }
  return true;
}
""",
    )
)

TS.append(
    Task(
        "ts_csv_line",
        "typescript",
        "algorithm",
        "Write a TypeScript function `parseCsvLine(line: string): string[]` that splits one CSV line "
        "into fields: fields are separated by commas; a field may be wrapped in double quotes, in "
        'which case it may contain commas, and a doubled quote ("") inside it means one literal '
        "quote. Unquoted fields are returned as-is (no trimming). An empty line is one empty field. "
        "Throw an Error for an unterminated quoted field." + TS_NOTE,
        '''
assert.deepEqual(parseCsvLine("a,b,c"), ["a", "b", "c"]);
assert.deepEqual(parseCsvLine(""), [""]);
assert.deepEqual(parseCsvLine("a,,c,"), ["a", "", "c", ""]);
assert.deepEqual(parseCsvLine('"x, y",z'), ["x, y", "z"]);
assert.deepEqual(parseCsvLine('"say ""hi""",2'), ['say "hi"', "2"]);
assert.deepEqual(parseCsvLine(' a , b '), [" a ", " b "]);
assert.deepEqual(parseCsvLine('""'), [""]);
assert.throws(() => parseCsvLine('"open,field'));
''',
        """
function parseCsvLine(line: string): string[] {
  const out: string[] = []; let i = 0;
  while (true) {
    let field = "";
    if (line[i] === '"') {
      i++;
      while (true) {
        if (i >= line.length) throw new Error("unterminated");
        if (line[i] === '"') {
          if (line[i + 1] === '"') { field += '"'; i += 2; } else { i++; break; }
        } else field += line[i++];
      }
    } else {
      while (i < line.length && line[i] !== ",") field += line[i++];
    }
    out.push(field);
    if (i >= line.length) return out;
    i++;
  }
}
""",
    )
)
