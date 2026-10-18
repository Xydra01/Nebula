# Technical Deep Dive 02: Flagship Python Language Module Specification

| Field | Value |
| --- | --- |
| Document | `docs/specs/language-modules/02-python-module-spec.md` |
| Component | `nebula-lang-python` (built-in or MCP server) |
| Engine | `uv` (Astral) |
| Status | Proposed Technical Specification |

---

## 1. Overview & Objectives

The Python Language Module (`nebula-lang-python`) is Nebula's flagship language pack. It solves the most common issues observed during autonomous Python development:
- **Zero Global Contamination**: Automatically boots an isolated `.venv` within each task's git worktree.
- **Fast, Deterministic Toolchain**: Utilizes `uv` for sub-second virtualenv provisioning, dependency resolution, and multi-version Python downloads.
- **Automated Virtualenv Routing**: Eliminates manual shell activation scripts by executing all scripts, tests, and tools directly through the worktree's `.venv\Scripts\` binaries.
- **Structured Verification**: Replaces unparsed console output with structured JSON test and lint diagnostics.

---

## 2. Environment Lifecycle & Virtualenv Provisioning

### 2.1 Provisioning Workflow (`uv venv`)
When a Python task begins, the module provisions a dedicated virtual environment inside the task's worktree:
1. **Target Directory**: `<worktree_root>\.venv`
2. **Version Pinning**: If `python_version` is provided in `TaskCreateParams.language_config` (e.g. `"3.12"`), the module passes `--python 3.12`. `uv` automatically downloads and provisions that Python version if not already present in the user cache. If omitted, it defaults to the system's pinned default (Python 3.12).
3. **Execution Command**:
   ```powershell
   uv venv --python <version> <worktree_root>\.venv
   ```
4. **Git Protection**: The module verifies that `.venv` is present in `<worktree_root>\.gitignore`. If missing, it automatically appends `.venv/` to `.gitignore` so temporary virtual environments are never committed to git branches.

### 2.2 Binary Paths
On Windows, binaries within the isolated environment are resolved at:
- Python Interpreter: `<worktree_root>\.venv\Scripts\python.exe`
- Pytest Runner: `<worktree_root>\.venv\Scripts\pytest.exe`
- Ruff Linter: `<worktree_root>\.venv\Scripts\ruff.exe`

---

## 3. Tool Specifications & Schemas

### 3.1 `python.env.ensure`
Verifies or provisions the task's virtual environment.

- **Permission Tier**: Tier 1 (Sandbox)
- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "python_version": {
        "type": "string",
        "enum": ["3.9", "3.10", "3.11", "3.12", "3.13"],
        "description": "Target Python minor version. Defaults to 3.12."
      }
    },
    "additionalProperties": false
  }
  ```
- **Output (JSON)**:
  ```json
  {
    "status": "ready",
    "python_version": "3.12.7",
    "venv_path": "F:\\Nebula\\worktrees\\01M54...\\.venv",
    "executable": "F:\\Nebula\\worktrees\\01M54...\\.venv\\Scripts\\python.exe"
  }
  ```

---

### 3.2 `python.pkg.add`
Installs packages into the task's virtual environment and updates dependency manifests.

- **Permission Tier**: Tier 1 (Sandbox)
- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "packages": {
        "type": "array",
        "items": { "type": "string" },
        "description": "List of package specifications (e.g. ['rich>=13.0', 'psutil'])"
      },
      "dev": {
        "type": "boolean",
        "description": "Whether to install as development dependencies."
      }
    },
    "required": ["packages"],
    "additionalProperties": false
  }
  ```
- **Behavior**:
  - If `pyproject.toml` exists: executes `uv add [--dev] <packages...>`.
  - If only `requirements.txt` exists: executes `uv pip install <packages...>` and appends the packages to `requirements.txt`.
  - If neither exists: creates a minimal `pyproject.toml` and executes `uv add <packages...>`.
- **Output (JSON)**:
  ```json
  {
    "installed": [
      { "name": "rich", "version": "13.9.2" },
      { "name": "psutil", "version": "6.0.0" }
    ],
    "manifest_updated": "pyproject.toml"
  }
  ```

---

### 3.3 `python.pkg.list`
Inspects all packages currently installed in the worktree's virtual environment.

- **Permission Tier**: Tier 0 (Read)
- **Input Schema**: `{ "type": "object", "properties": {}, "additionalProperties": false }`
- **Output (JSON)**:
  ```json
  {
    "packages": [
      { "name": "pytest", "version": "8.3.3" },
      { "name": "rich", "version": "13.9.2" },
      { "name": "psutil", "version": "6.0.0" }
    ]
  }
  ```

---

### 3.4 `python.run`
Executes a Python script directly within the virtual environment without manual activation.

- **Permission Tier**: Tier 1 (Sandbox)
- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "script": {
        "type": "string",
        "description": "Relative path to the python script within the worktree."
      },
      "args": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Optional command-line arguments passed to the script."
      }
    },
    "required": ["script"],
    "additionalProperties": false
  }
  ```
- **Execution**: Spawns `<worktree_root>\.venv\Scripts\python.exe <script> <args...>` under the task's Job Object, with `current_dir = worktree_root`.
- **Smart Remediation on Failure**: If standard error contains `ModuleNotFoundError: No module named '<pkg>'`, the tool result appends a remediation hint:
  `"Hint: Package '<pkg>' is not installed in the virtual environment. Call python.pkg.add(['<pkg>']) to install it."`

---

### 3.5 `python.test.run`
Executes `pytest` within the virtual environment and returns structured test results.

- **Permission Tier**: Tier 1 (Sandbox)
- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "test_path": {
        "type": "string",
        "description": "Optional specific test file or directory (e.g. 'tests/test_storage.py')."
      },
      "args": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Optional additional pytest arguments."
      }
    },
    "additionalProperties": false
  }
  ```
- **Behavior**:
  - Automatically ensures `pytest` is present in the `.venv` (installs via `uv pip install pytest` if missing).
  - Executes `pytest -q --tb=short` with output parsed into structured test summaries.
- **Output (Structured JSON)**:
  ```json
  {
    "status": "failed",
    "passed": 3,
    "failed": 1,
    "errors": 0,
    "duration_s": 0.45,
    "failures": [
      {
        "test": "tests/test_storage.py::test_free_space_calculation",
        "file": "tests/test_storage.py",
        "line": 42,
        "message": "AssertionError: assert 1024 == 2048"
      }
    ]
  }
  ```

---

### 3.6 `python.lint.run`
Executes `ruff` within the virtual environment for fast, structured static analysis.

- **Permission Tier**: Tier 0 (Read)
- **Input Schema**:
  ```json
  {
    "type": "object",
    "properties": {
      "path": {
        "type": "string",
        "description": "Directory or file to lint; defaults to worktree root."
      },
      "fix": {
        "type": "boolean",
        "description": "Automatically apply safe fixes where possible."
      }
    },
    "additionalProperties": false
  }
  ```
- **Output (Structured JSON)**:
  ```json
  {
    "status": "clean",
    "findings_count": 0,
    "findings": []
  }
  ```

---

## 4. Prompt Guidelines Injected for Python Tasks

When the Python Language Module is active, the executor's system prompt receives this specialized context block:

```text
--- PYTHON LANGUAGE MODULE ACTIVE ---
Environment & Virtualenv:
- A dedicated Python virtual environment is active in your worktree at '.venv\'.
- DO NOT attempt to run global python or manual activation scripts (e.g. 'activate.ps1').
- Use 'python.run' to execute your scripts; it automatically runs inside the virtual environment.
- Use 'python.pkg.add' to install third-party libraries (e.g. requests, rich, psutil) into the virtual environment. Never install packages globally.
- Use 'python.test.run' to run test suites with pytest. It returns structured per-test pass/fail results.
- Use 'python.lint.run' to check your code with ruff.
- Use 'fs.write', 'fs.read', and 'fs.list' for all file modifications.
```
