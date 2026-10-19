# Nebula Prompt Translation Layer: The Multi-Language Rosetta Stone Specification

**Document Version:** 2.0.0  
**Status:** Approved Reference Specification & Translation Core  
**Target Subsystem:** Nebula Prompt Preprocessor & Translation Middleware (`nebula-cli` / `nebula-daemon`)  
**Underlying Model Validation:** Bonsai 2 27B PQ2_0 (llama-prism / standard) on RTX 4070 (12 GB)  
**Empirical Evidence:** Live A/B Trials across Python, Rust, and C++ (`scratch/ab_experiment_results.json`, `scratch/compiled_lang_results.json`)

---

## 1. Architectural Purpose & Decision Record

The **Nebula Translation Layer** is an automated semantic compiler situated between raw human input (CLI `nebula task run "<input>"` or TUI prompt input) and the task executor loop (`nebula-daemon::executor`).

```
┌────────────────────────────────────────────────────────┐
│               Raw Human Input (CLI / TUI)              │
│  "compile and fix the c++ code so the tests pass"      │
│  "fix the rust bug so cargo test passes"               │
└───────────────────────────┬────────────────────────────┘
                            │
                            ▼
┌────────────────────────────────────────────────────────┐
│            Non-Deterministic LLM Translator            │
│          (1.1s – 1.7s Latency Budget Preflight)        │
│    • Semantic Entity & Target Path Discovery           │
│    • Language-Specific Idiom Inoculation               │
│    • Framework Gap Bridging (CMake / Cargo / Devtools) │
│    • Closed-Loop Verification Gate Synthesis           │
└───────────────────────────┬────────────────────────────┘
                            │
                            ▼
┌────────────────────────────────────────────────────────┐
│             Compiled Execution Contract                │
│   • Explicit target paths (include/, src/, tests/)     │
│   • Multi-stage build sequence (cmake -B / cargo test) │
│   • Non-interactive assertion & error handling         │
│   • Zero-panic & strict typing compliance              │
│   • Deterministic Definition of Done (DoD)             │
└───────────────────────────┬────────────────────────────┘
                            │
                            ▼
┌────────────────────────────────────────────────────────┐
│             Nebula Autonomous Executor Loop            │
│           (fs.read, fs.write, shell.run, git)          │
└────────────────────────────────────────────────────────┘
```

### Architectural Decision: Non-Deterministic LLM Translation vs. Deterministic Matching

Early exploration considered a two-tier hybrid model using deterministic regular expression / AST matching as the fast path. **Empirical testing led to the deprecation of deterministic parsing in favor of pure LLM translation:**

1. **Failure Modes of Deterministic Matchers:**
   - Real-world human prompts are colloquial, fragmented, or multi-clause (e.g. *"check the math code, fix the weird edge case in median and make sure the tests pass without messing up anything else"*).
   - Regex/AST systems suffer from severe false positives (triggering incorrect templates) or brittle false negatives (falling through to unguided raw execution).
2. **Empirical Latency Budget Analysis:**
   - On the local RTX 4070 hardware (Bonsai 2 27B profile), zero-shot LLM translation requires:
     - Prompt ingestion (250–550 ms for ~200 context tokens)
     - Generation (600–1,100 ms for 50–90 completion tokens)
     - **Total Preflight Translation Latency: 1.1s to 1.7s**
   - In contrast, the average Nebula autonomous task loop requires **10s to 60s+**.
   - Spending 1.5 seconds upfront constitutes a negligible ~3–5% latency overhead, but increases task completion and test pass rates from **0% (Raw C++ / Greenfield Python) to 100%**.
   - The non-deterministic nature of the LLM provides fluid adaptation across conversational ambiguities that no deterministic ruleset can match.

---

## 2. Multi-Language Idiom Gaps & Invariants

Empirical trials across Python, Rust, and C++ revealed distinct failure modes where models generate code that looks plausible but fails compilation, breaks testing harnesses, or violates safety invariants.

### A. Rust Idiom Gaps & Contract Invariants

| Problem Area | Raw Model Failure Mode | Rosetta Stone Contract Invariant |
|---|---|---|
| **Float Ordering** | Calls `sorted.sort()` (compilation error: `f64` does not implement `Ord`) or `sorted.sort_by(\|a, b\| a.partial_cmp(b).unwrap())` | Mandate `.sort_by(\|a, b\| a.total_cmp(b))`. Never use `.unwrap()` in library code (`AGENTS.md` zero-panic rule). |
| **Borrow Checker & Mutability** | Attempts in-place mutation of slice `numbers` or omits `mut` on vector clone. | Explicitly mandate `let mut sorted = numbers.to_vec();`. |
| **Toolchain Invocation** | Runs `rustc src/lib.rs` directly via `shell.run`, failing due to missing crate types or dependencies. | Mandate standard `cargo test` and `cargo check`. Prohibit standalone `rustc` invocations. |
| **Step Budget Conservation** | In verbose contracts, models execute pre-reproduction test runs (`cargo test`), diagnostic tool queries (`where cargo`), burning 4+ steps before patching. | Streamline contract to: `Inspect -> Patch -> Validate`. Reserve tool calls for patch application and final validation gate. |

### B. C++ Idiom Gaps & Contract Invariants

| Problem Area | Raw Model Failure Mode | Rosetta Stone Contract Invariant |
|---|---|---|
| **Out-of-Source Builds** | Fresh worktrees lack a `build/` directory. Naive prompts cause models to run `cmake --build build` (fails: cache not found) or invoke `cl` without include flags. | Mandate the two-stage CMake lifecycle:<br>1. Configure: `cmake -B build -S .`<br>2. Build: `cmake --build build --config Debug` |
| **Windows MSVC GUI Popups** | Using standard `<cassert>` in MSVC debug builds invokes `_wassert`, displaying an interactive Abort/Retry/Ignore GUI modal dialog that permanently hangs headless agents. | Translation layer enforces non-interactive test macros that output to `stderr` and exit with non-zero status codes on failure. |
| **Header / Source Separation** | Models attempt to place full implementations inside `.hpp` headers or lose namespace scoping in `.cpp`. | Explicitly reference both `include/<name>.hpp` and `src/<name>.cpp` with appropriate namespace qualifications (`namespace stats { ... }`). |
| **Standard Library Sorting** | In-place sorting of `const std::vector<double>&` fails. | Explicitly require copying to a mutable vector: `std::vector<double> vec(numbers); std::sort(vec.begin(), vec.end());`. |

### C. Other Ecosystems (TypeScript / UI & SQL / Database)

| Ecosystem | Identified Idiom Gap | Rosetta Stone Contract Invariant |
|---|---|---|
| **TypeScript / Node / UI** | Edits pass syntax checks but fail type checking; models ignore path aliases (`@/components`) or run unconfigured bundlers. | Mandate a two-phase check: (1) Typecheck: `npx tsc --noEmit`, (2) Test runner: `npm test` or `npx vitest run`. Respect `tsconfig.json` paths. |
| **SQL & Migrations** | Models modify existing, already-applied historical migration files, corrupting database state. | Enforce append-only migration policy: Generate new forward migration files (`V<timestamp>__<name>.sql`). Validate both `up` execution and `down` rollback. |

---

## 3. Nebula Framework Gaps & Empirical Workarounds

During live testing on the Nebula daemon and toolchain, several internal framework gaps were identified that hinder automated testing. The Rosetta Stone translation layer actively bridges these gaps:

### Framework Gap 1: Missing C++ Adapter in `nebula-devtools`
- **Issue:** `crates/nebula-devtools` only implements `PythonAdapter` and `RustAdapter`. There is no native `CppAdapter` for MCP `test.run` or `build.run`.
- **Empirical Impact:** A raw prompt asking to "run tests" on C++ fails to resolve an MCP test tool.
- **Rosetta Workaround:** The translation layer detects C++ projects (via `CMakeLists.txt` or `.cpp`/`.hpp` files) and compiles testing directives into explicit `shell.run` commands driving the two-stage CMake lifecycle (`cmake -B build -S .` followed by `cmake --build build --config Debug` and `.\build\Debug\<test_binary>`).

### Framework Gap 2: `RustAdapter.test` Dependency on `cargo-nextest`
- **Issue:** The internal `RustAdapter.test` implementation invokes `cargo nextest run`. On systems where `cargo-nextest` is not installed, it returns a tool error.
- **Empirical Impact:** Calling `test.run` fails on standard Rust installations.
- **Rosetta Workaround:** The translation layer directs Rust test execution via `shell.run` using standard `cargo test`, which is universally supported by the default Rust toolchain.

### Framework Gap 3: Environment Scrubbing & Subprocess `PATH` Isolation (`CONTROLLED_PATH`)
- **Issue:** In `crates/nebula-tools/src/builtins/shell.rs`, `shell.run` enforces security requirement 4.7 by scrubbing the environment and pinning `PATH` to `C:\Windows\System32;C:\Windows`. The initial command (e.g. `cargo.exe`, `cmake.exe`) is resolved via the host parent PATH, but child processes inherit only `CONTROLLED_PATH`.
- **Empirical Impact:** Any exploratory diagnostic commands invoked by models—such as `where cargo`, `where rustc`, or `which python`—fail with exit code 1 inside the child process. Unguided models interpret this as a missing toolchain and enter an exploratory death spiral.
- **Rosetta Workaround:** The translation layer explicitly forbids exploratory `where`/`which` queries and injects exact, canonical toolchain invocations.

### Framework Gap 4: Security Tier Escalation on Toolchains
- **Issue:** Invoking `cargo`, `rustc`, `cmake`, or custom test binaries via `shell.run` trips `Tier::System` (`<no-match>` in `rules.toml`), triggering a 5-minute approval timeout unless approved by a human or daemon.
- **Empirical Impact:** Tasks stall indefinitely in unattended environments.
- **Rosetta Workaround:** The translation layer structures tool invocations with clear command prefixes and arguments, allowing automated approval policies or daemon rules to match known build/test commands.

---

## 4. Empirical Benchmark Proof Matrix

Ground truth data captured from live runs on `F:\Nebula` across all three language environments:

| Trial ID | Language | Prompt Condition | Tool Calls | Duration | Worktree Test Result | Code Quality & Safety Compliance |
|---|---|---|---|---|---|---|
| **`AB_T1_Raw`** | Python | Raw Human Prompt | 8 | 15.2s | ❌ Failed (0 tests) | 0 target files created; wasted steps on unguided exploration |
| **`AB_T1_Trans`** | Python | Rosetta Translated | 6 | 13.8s | ✅ **100% Pass** (10 tests) | Created `cleaner.py` + `test_cleaner.py` using `TemporaryDirectory` |
| **`AB_T2_Raw`** | Python | Raw Human Prompt | 8 | 14.1s | ❌ Failed (Import Error) | Regression: omitted `calculate_stdev`, broke existing test suite |
| **`AB_T2_Trans`** | Python | Rosetta Translated | 7 | 14.5s | ✅ **100% Pass** (23 tests) | All functions implemented; 23/23 unit tests passing cleanly |
| **`Rust_Raw`** | Rust | Raw Human Prompt | 8 | 9.89s | ⚠️ Accidental Pass | **Violates AGENTS.md**: used `.unwrap()` on `partial_cmp(b)`; never ran tests |
| **`Rust_Trans_V`** | Rust | Rosetta (Pre-reproduce) | 8 | 8.47s | ❌ Limit Reached | Stalled running standalone `rustc` queries during pre-reproduction |
| **`Rust_Trans_D`** | Rust | Rosetta (Direct Patch) | 6 | 11.20s | ✅ **100% Pass** (3 tests) | **Zero-panic compliance**: uses `.total_cmp(b)`; verified under `cargo test` |
| **`Cpp_Raw`** | C++ | Raw Human Prompt | 6 | 5.02s | ❌ Failed (0 tests) | Stalled on directory listing; never edited `stats.cpp` or invoked `cmake` |
| **`Cpp_Trans`** | C++ | Rosetta Translated | 8 | 17.19s | ✅ **100% Pass** (4 tests) | Two-stage CMake configure + build + non-interactive test binary pass |

---

## 5. Multi-Language Rosetta Stone Translation Grammar

When raw user input arrives, the LLM Translation Preflight translates it according to this language-aware contract grammar:

### Core Contract Schema
```text
Goal: [Clear, unambiguous operational goal identifying the primary artifact]

Language Idioms & Verification Contract:
1. Framework Adaptation: [Specify toolchain or note missing native devtools adapter]
2. Inspection: [Explicitly name target files to read using fs.read]
3. Implementation / Patch:
   - [Language-specific typing, mutability, and safety requirements]
   - [Zero-panic / exception / error handling contracts]
4. Build & Lifecycle Management:
   - [Exact build / configure sequence: e.g. cargo check or cmake -B / cmake --build]
5. Verification Gate:
   - [Exact test command: e.g. cargo test or .\build\Debug\<test_bin>]
   - [Pass criteria: 0 failures, 100% assertions satisfied]
6. Definition of Done: Once the test verification command exits with 0 and passes all assertions, you must output your final deliverable summary in plain text and conclude immediately without invoking any further tools.
```

---

## 6. Real-World Translation Catalog (Multi-Language)

### Catalog Entry 1: Rust Defect Resolution
#### ❌ Raw Input:
```text
fix the rust bug so cargo test passes
```
#### ✅ Rosetta Translated Contract:
```text
Goal: Fix the median calculation in src/lib.rs so that all test suites pass under cargo test.

Language Idioms & Verification Contract:
1. Inspection: Read src/lib.rs and tests/test_stats.rs using fs.read.
2. Patch: In src/lib.rs, rewrite median(&[f64]) -> f64:
   - Return 0.0 if empty.
   - Clone into a mutable Vec<f64> and sort using .sort_by(|a, b| a.total_cmp(b)) (Rust floats do not implement Ord; never use .unwrap() in library code).
   - Return sorted[len / 2] if odd; return (sorted[len / 2 - 1] + sorted[len / 2]) / 2.0 if even.
3. Verification Gate: Run shell.run with 'cargo test'. Verify 0 failures.
4. Definition of Done: Once cargo test exits with code 0 and all assertions pass, output your final deliverable summary in plain text and conclude immediately without invoking further tools.
```

### Catalog Entry 2: C++ Out-of-Source Build & Defect Resolution
#### ❌ Raw Input:
```text
compile and fix the c++ code so the tests pass
```
#### ✅ Rosetta Translated Contract:
```text
Goal: Fix the median calculation in src/stats.cpp and verify that test_stats passes.

Language Idioms & Verification Contract:
1. Framework Gap Note: Nebula does not have a native C++ build adapter; execute all compilation and test steps via shell.run.
2. Inspection: Read include/stats.hpp, src/stats.cpp, and tests/test_stats.cpp using fs.read.
3. Patch: In src/stats.cpp, fix stats::median(const std::vector<double>& numbers):
   - Return 0.0 if empty.
   - Copy into a mutable std::vector<double> and sort using std::sort(vec.begin(), vec.end()).
   - Return vec[n / 2] if odd; return (vec[n / 2 - 1] + vec[n / 2]) / 2.0 if even.
4. Two-Stage CMake Lifecycle:
   - Configure: Run shell.run with 'cmake -B build -S .'
   - Compile: Run shell.run with 'cmake --build build --config Debug'
5. Test Verification: Run shell.run with '.\build\Debug\test_stats.exe'.
6. Definition of Done: Once test_stats.exe exits with code 0 ("All C++ stats tests passed successfully!"), output your final deliverable summary in plain text and conclude immediately without invoking further tools.
```

---

## 7. Runtime LLM Translation System Prompt

The translation layer will execute with this system prompt on the local Nebula LLM:

```text
You are the Nebula Prompt Translation Compiler.
Your role is to translate raw, ambiguous, or colloquial human requests into rigorous, language-aware Nebula Execution Contracts.

Operational Principles:
1. Explicit Grounding: Never allow the agent to infer file paths. Inject canonical filenames based on repository context.
2. Idiom Inoculation: Apply strict language rules (e.g., total_cmp and zero-panic in Rust, two-stage CMake and non-interactive asserts in C++, typechecking in TypeScript).
3. Framework Bridging: When framework gaps exist (e.g. missing C++ devtools adapter), provide the exact shell.run command sequence.
4. Closed-Loop Gate & Voluntary Exit Directive: Every modifying contract must culminate in an observable verification command that passes with 0 errors, immediately followed by the mandatory exit directive instructing the agent to output its final summary and conclude immediately without invoking further tool calls.
5. Conciseness: Avoid superfluous pre-reproduction steps that consume limited tool-call budgets. Proceed directly: Inspect -> Patch -> Validate.

Output exclusively the compiled contract markdown block.

```

---

## 8. System Execution Invariants & Architectural Risk Mitigations

To bridge the gap between prompt contract synthesis and runtime task execution, the translation layer and executor loop enforce three binding system invariants:

### Invariant 1: Voluntary Task Termination, Auto-Commit, & Lock Contention Protocol (Issue #69, #72)
- **Problem:** Prompt-level exit directives ("output summary and conclude") can be ignored by local models, leading to runaway diagnostic tool loops that end in `LimitReached` rather than `Completed`. Furthermore, if background indexers hold `.git/index.lock`, auto-commit could fail silently.
- **System Contract:**
  1. Builtin `task.finish`: The executor exposes an explicit `task.finish(summary, status)` tool. Once verification succeeds, the prompt directs calling `task.finish`.
  2. Git Auto-Commit Invariant: Calling `task.finish` immediately triggers the worktree git auto-commit hook (`task.worktree_committed`). Uncommitted changes in the task worktree are unconditionally committed to git before transitioning to state `Completed`.
  3. Three-Tier Windows NTFS Lock Contention Protocol:
     - **Tier 1 (Exponential Backoff Retries)**: If `.git/index.lock` is held by a transient read scanner (Windows Search Indexer, antivirus), the auto-commit hook retries `remove_file` up to 5 times over 1.5s (25ms, 50ms, 100ms, 250ms, 500ms) to allow the handle to clear.
     - **Tier 2 (NTFS Atomic Quarantine Rename)**: If `remove_file` fails due to `ERROR_SHARING_VIOLATION` (access denied), the hook attempts `std::fs::rename` to `.git/index.lock.<timestamp>.stale`. On Windows NTFS, renaming an open file with delete-sharing permissions succeeds where deletion fails, instantly freeing the canonical `.git/index.lock` path for git operations.
     - **Tier 3 (Diagnostic Error Reporting & Propagation)**: If both deletion and quarantine rename fail, `task.finish` **must not** drop changes or silently finalize. It returns `ToolError::Unavailable` to the agent reporting Win32 error 32 (`ERROR_SHARING_VIOLATION`), the exact lock path, and actionable remediation (exclude `F:\Nebula\worktrees` from Windows Search Indexing / antivirus monitoring).
  4. Subprocess Concurrency Barrier (Issue #79):
     - If a model emits multiple tool calls in a single turn where one is `task.finish` alongside background child commands, active child processes in the task's Windows Job Object (`CURRENT_TASK_JOB` via `JobChild` in `crates/nebula-tools/src/builtins/task_job_seam.rs`) are terminated before worktree auto-commit executes.

### Invariant 2: Security Ruleset Implementation Ordering & Dependency Enforcement (Issue #71, #74, #75, #80)
- **Problem:** Adding unanchored regex whitelists for build tools (`cargo`, `cmake`) to `rules.toml` without prior path validation can introduce sandbox escapes. GitHub does not natively enforce issue dependency DAGs on merge.
- **System Contract:**
  1. Implementation Gate: Issue #71 is strictly blocked by Issue #74. No PR modifying `rules.toml` or `config/default.toml` may be merged until red-team property tests in `crates/nebula-sandbox/src/redteam.rs` are in place.
  2. PR Template Enforcement: `.github/pull_request_template.md` and `CONTRIBUTING.md` enforce an explicit human checklist requirement verifying that #74 is merged into `main` before reviewing #71.
  3. Strict Worktree Anchoring: Whitelist rules must enforce that working directories and file arguments resolve strictly within `<worktree_root>`, escalating any out-of-worktree or traversal (`..`) attempts to `Tier::System`.
  4. Automated Dependency Gate & Rate-Limit Resilience (Issue #75, #80): `.github/workflows/pr-dependency-gate.yml` verifies prerequisite dependency issues automatically via `scripts/check-pr-dependencies.ps1` with path filters, exponential retry on API 403, and offline local bypass (`NEBULA_SKIP_DEPENDENCY_NETWORK_CHECK=1`).

### Invariant 3: MCP Process Lifecycle & Lazy Post-Detection Pre-warming (Issue #65, #70, #73)
- **Problem:** External language servers (like `nebula-lang-cpp`) spawned unconditionally across all tasks waste RAM/VRAM on non-C++ workloads. If spawned on-demand, cold-start toolchain discovery (`vswhere`, MSVC environment probing) can exceed `call_timeout` (30s).
- **System Contract:**
  1. Zero Idle Overhead: `nebula-lang-cpp` is **never** spawned globally or for non-C++ tasks (e.g. pure Python or Rust).
  2. Lazy Post-Detection Pre-warming: The daemon inspects repository signatures during initial task setup (`nebula-daemon::executor::language.detected`). If `language == "cpp"` or `"polyglot"`, the daemon immediately spawns `nebula-lang-cpp` in the background during the initial prompt ingestion step (~1.5s).
  3. Cold-Start Bounds & Caching: Initial toolchain discovery completes in `<2.0s` while the model reads context. Once warm, subsequent tool invocations execute in `<50ms` within the task's shared Job Object.


