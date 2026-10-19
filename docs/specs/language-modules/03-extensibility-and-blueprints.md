# Technical Deep Dive 03: Extensibility Contract & Module Blueprints (C++, SQL, Rust)

| Field | Value |
| --- | --- |
| Document | `docs/specs/language-modules/03-extensibility-and-blueprints.md` |
| Component | Module authoring interface & blueprints for C++, SQL, Rust |
| Status | Proposed Technical Specification |

---

## 1. The Language Module Contract

To maintain structural predictability and prevent context degradation for local models, any new language pack must satisfy the **Language Module Contract**:

1. **Explicit Isolation**: Must operate within a designated task worktree boundary (`F:\Nebula\worktrees\<task-id>\`). Cannot write to global paths or pollute host system directories.
2. **Standardized Tool Naming**: Tools must follow `<language>.<domain>.<verb>` (e.g., `cpp.build.compile`, `sql.query.execute`, `python.pkg.add`).
3. **Structured Diagnostics First**: Must parse compiler, test, or linter streams into structured JSON objects containing `file`, `line`, `col`, and `message`. Unparsed terminal noise must be relegated to raw log blobs.
4. **Job Object Containment**: All child processes must be enrolled in the task's Windows Job Object with `KILL_ON_JOB_CLOSE`.
5. **No Shell Guessing**: Language-specific actions must be performed via module tools rather than relying on the LLM to emit raw command strings.

---

## 2. Blueprint 1: C++ Language Module (`nebula-lang-cpp`)

### 2.1 Toolchain Discovery on Windows
On Windows, C++ toolchains are disparate and complex. The module automatically resolves installed toolchains without user intervention:
- **MSVC Toolchain**: Discovered via `vswhere.exe`:
  ```text
  %ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64
  ```
  The module initializes environment variables (`INCLUDE`, `LIB`, `PATH`) dynamically using `vcvarsall.bat x64` in a headless probe.
- **Clang / LLVM**: Checks for `clang-cl.exe` and `lld-link.exe` in `PATH`.
- **Build Generators**: Verifies `cmake.exe` and `ninja.exe`.

### 2.2 Dedicated Tools

#### `cpp.build.configure`
Configures a CMake build tree in `<worktree_root>\build\`.
- **Permission Tier**: Tier 1 (Sandbox)
- **Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "generator": { "type": "string", "enum": ["Ninja", "Visual Studio 17 2022"], "default": "Ninja" },
      "build_type": { "type": "string", "enum": ["Debug", "Release"], "default": "Debug" },
      "cmake_args": { "type": "array", "items": { "type": "string" } }
    },
    "additionalProperties": false
  }
  ```

#### `cpp.build.compile`
Compiles targets and returns structured compiler diagnostics.
- **Permission Tier**: Tier 1 (Sandbox)
- **Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "target": { "type": "string", "description": "CMake target to build; builds all if omitted." },
      "clean_first": { "type": "boolean", "default": false }
    },
    "additionalProperties": false
  }
  ```
- **Diagnostic Parsing**:
  MSVC errors matching `(?P<file>.+)\((?P<line>\d+),(?P<col>\d+)\):\s+(?P<severity>error|warning)\s+(?P<code>C\d+):\s+(?P<msg>.+)` are extracted into:
  ```json
  {
    "status": "failed",
    "error_count": 1,
    "warning_count": 0,
    "diagnostics": [
      {
        "file": "src/storage_manager.cpp",
        "line": 84,
        "col": 12,
        "severity": "error",
        "code": "C2065",
        "message": "'GetDiskFreeSpaceEx': undeclared identifier. Did you forget #include <windows.h>?"
      }
    ]
  }
  ```

#### `cpp.test.run`
Executes test suites via `ctest` or Catch2/GoogleTest binaries.
- **Permission Tier**: Tier 1 (Sandbox)
- **Output**: JSON report of test pass/fail counts and specific assertion failures.

### 2.3 Architectural Boundary & Process Lifecycle (#65, #70, #73)
- **MCP Server vs. Devtools Adapter Boundary**:
  - `nebula-lang-cpp` (Issue #65) is the authoritative out-of-process MCP language server providing full CMake project introspection, toolchain detection, and MSVC/Clang diagnostic parsing.
  - `crates/nebula-devtools::CppAdapter` (Issue #70) serves as the in-crate consumer implementing `BuildAdapter` and `TestAdapter`. It coordinates directly over MCP or via a shared toolchain discovery crate to prevent code duplication.
- **Process Lifecycle & Lazy Post-Detection Pre-warming**:
  - **Zero Idle Overhead**: `nebula-lang-cpp` is never spawned globally or for non-C++ tasks, keeping RAM and VRAM completely free for Python, Rust, or general tasks.
  - **Post-Detection Trigger**: When `language.detected` identifies C++ or polyglot signatures, the daemon initiates lazy background pre-warming of `nebula-lang-cpp` during the model's initial prompt read phase (~1.5s).
  - **Cold-Start Latency Budget**: Toolchain discovery (`vswhere`, MSVC environment initialization) must complete or be cached within `<2.0s`. Subsequent tool invocations execute in `<50ms` within the task's shared Windows Job Object.
- **Hermetic Build Tree Standard**:
  - All CMake build trees reside strictly at `<worktree_root>\build\`, ensuring that clean builds, cache invalidation, and Job Object cleanup operate within the task's isolated confinement root.

---

## 3. Blueprint 2: SQL & Database Language Module (`nebula-lang-sql`)

### 3.1 Sandboxed Execution Environment
Autonomous database work introduces risks of data destruction or schema drift. The SQL module introduces sandbox barriers:
1. **SQLite Sandboxing**: Automatic creation of isolated `.sqlite3` databases inside the task worktree.
2. **Schema Introspection**: Allows the model to inspect existing tables, types, and constraints before writing queries.
3. **Rollback-by-Default Query Testing**: Queries and migrations are verified inside a transaction that is rolled back after validation.

### 3.2 Dedicated Tools

#### `sql.schema.inspect`
Returns the current database schema as structured JSON.
- **Permission Tier**: Tier 0 (Read)
- **Output**:
  ```json
  {
    "tables": [
      {
        "name": "drives",
        "columns": [
          { "name": "id", "type": "INTEGER", "primary_key": true },
          { "name": "mount_point", "type": "TEXT", "nullable": false },
          { "name": "free_bytes", "type": "BIGINT", "nullable": false }
        ],
        "foreign_keys": []
      }
    ]
  }
  ```

#### `sql.query.execute`
Runs a read-only query against the database with strict row and execution time limits.
- **Permission Tier**: Tier 1 (Sandbox)
- **Input**: `{ "query": "SELECT mount_point, free_bytes FROM drives WHERE free_bytes < 10737418240;", "limit": 50 }`
- **Output**: Formatted result rows and column headers. Rejects mutations (e.g. `DROP`, `DELETE`, `UPDATE`) unless invoked through migration tools.

#### `sql.migrate.run`
Applies migration scripts in order within a sandboxed verification transaction.
- **Permission Tier**: Tier 1 (Sandbox)
- **Input**: `{ "migration_dir": "migrations/" }`
- **Output**: List of applied migrations and verification status.

#### `sql.lint.run`
Runs `sqlfluff` to ensure SQL syntax conforms to dialect standards (PostgreSQL, SQLite, MySQL) and stylistic rules.
- **Permission Tier**: Tier 0 (Read)

---

## 4. Blueprint 3: Rust Language Module (`nebula-lang-rust`)

### 4.1 Target Cache & Job Isolation
Rust compilation times and disk commit charge can be substantial on Windows. The Rust module configures:
- Shared target cache via `CARGO_TARGET_DIR=F:\Nebula\build\target` with `sccache` integration.
- Concurrency bounding (`-j 2`) to protect commit charge on Windows workstations.

### 4.2 Dedicated Tools
- `rust.build.run`: Runs `cargo build --message-format=json` and parses compiler diagnostics into structured file:line entries.
- `rust.test.run`: Invokes `cargo nextest run` and returns structured test results.
- `rust.lint.run`: Invokes `cargo clippy -- -D warnings` and captures linter suggestions.

---

## 5. Comparison Matrix: Language Packs vs. Raw LLM Shell

| Capability | Raw LLM Shell Execution | Dedicated Language Modules |
|---|---|---|
| **Environment Isolation** | Global system contamination; broken paths | Hermetic virtual environment / build tree per task |
| **Toolchain Discovery** | Hallucinates C: paths, fails on WindowsApps shims | Automatic resolution (`uv`, `vswhere`, `cargo`) |
| **Failure Feedback** | Raw ANSI stream dump; model context blown | Structured JSON diagnostics (`file:line:col`, error code) |
| **Dependency Management** | Manually tries `pip install`, often misses venv | Dedicated tool (`python.pkg.add`) updates manifests |
| **Verification Gate** | LLM guesses if tests passed from terminal chatter | Objective pass/fail metrics from structured test runners |
| **Prompt Footprint** | Bloated with polyglot instructions | Scoped exclusively to active language tenets |
