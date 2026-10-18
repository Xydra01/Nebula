# Technical Deep Dive 01: Core Language Module Architecture & Daemon Integration

| Field | Value |
| --- | --- |
| Document | `docs/specs/language-modules/01-core-architecture.md` |
| Component | `nebula-daemon`, `nebula-tools`, `nebula-proto`, `nebula-cli` |
| Status | Proposed Technical Specification |

---

## 1. Overview & Motivation

The Core Language Module Architecture provides the runtime substrate for language-specific tool specialization, environment isolation, and prompt customization in Nebula. Instead of treating every task as a generic shell session, the daemon pairs each task worktree with a dedicated `LanguageModule` implementation that provisions isolated environments, mounts tailored MCP tools, and injects authoritative instructions.

---

## 2. Rust Trait Definition: `LanguageModule`

Each language module implements the `LanguageModule` asynchronous trait. The trait lifecycle maps cleanly onto task initialization, inner-loop execution, and task finalization:

```rust
use std::path::Path;
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::Value;

use nebula_tools::{ToolDescriptor, ToolHost};
use crate::error::DaemonResult;

/// Metadata describing a registered language module.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LanguageMetadata {
    pub id: String,                    // e.g., "python", "cpp", "sql", "rust"
    pub display_name: String,          // e.g., "Python 3.x"
    pub description: String,           // Short summary of capabilities
    pub supported_versions: Vec<String>, // e.g., ["3.10", "3.11", "3.12", "3.13"]
    pub default_version: Option<String>,
}

/// The environment and configuration context passed during module provisioning.
pub struct ProvisionContext<'a> {
    pub task_id: &'a str,
    pub worktree_root: &'a Path,
    pub options: Option<Value>,       // Module-specific options from TaskCreateParams
    pub job_handle: Option<windows::Win32::Foundation::HANDLE>, // Windows Job Object
}

/// Verification report returned when a language module finishes final checks.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FinalizeReport {
    pub success: bool,
    pub summary: String,
    pub artifacts: Vec<String>,
}

#[async_trait]
pub trait LanguageModule: Send + Sync {
    /// Unique identifier of the language module (e.g. "python").
    fn id(&self) -> &str;

    /// Return metadata describing the module.
    fn metadata(&self) -> LanguageMetadata;

    /// Check if the target repository/worktree matches this language.
    /// Used for automatic language detection when the user does not specify --lang.
    async fn detect(&self, repo_root: &Path) -> Option<f32>; // Score 0.0 .. 1.0

    /// Provision the hermetic environment (e.g., .venv, CMake build dir) in the worktree.
    async fn provision(&self, ctx: &ProvisionContext<'_>) -> DaemonResult<()>;

    /// Return the list of tools provided by this module for the task.
    fn tools(&self) -> Vec<ToolDescriptor>;

    /// System prompt instructions and constraints specific to this language module.
    fn prompt_guidelines(&self, ctx: &ProvisionContext<'_>) -> String;

    /// Run post-task verification (formatting, lockfile cleanup, final test passes).
    async fn finalize(&self, ctx: &ProvisionContext<'_>) -> DaemonResult<FinalizeReport>;
}
```

---

## 3. Daemon Language Registry: `LanguageRegistry`

The `LanguageRegistry` manages registered language modules inside `nebula-daemon`:

```rust
use std::collections::HashMap;
use std::sync::Arc;

pub struct LanguageRegistry {
    modules: HashMap<String, Arc<dyn LanguageModule>>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        Self { modules: HashMap::new() }
    }

    pub fn register(&mut self, module: Arc<dyn LanguageModule>) {
        self.modules.insert(module.id().to_owned(), module);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn LanguageModule>> {
        self.modules.get(id).cloned()
    }

    pub fn list(&self) -> Vec<LanguageMetadata> {
        self.modules.values().map(|m| m.metadata()).collect()
    }

    /// Auto-detect the best language module for a given repository.
    pub async fn detect(&self, repo_root: &Path) -> Option<Arc<dyn LanguageModule>> {
        let mut best: Option<(Arc<dyn LanguageModule>, f32)> = None;
        for module in self.modules.values() {
            if let Some(score) = module.detect(repo_root).await {
                if score > 0.0 && best.as_ref().map_or(true, |(_, s)| score > *s) {
                    best = Some((module.clone(), score));
                }
            }
        }
        best.map(|(m, _)| m)
    }
}
```

---

## 4. Wire Protocol Updates (`nebula-proto`)

To support language selection and module introspection, `crates/nebula-proto/` is extended:

### 4.1 `TaskCreateParams` Additions
```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TaskCreateParams {
    pub goal: String,
    pub repo: std::path::PathBuf,
    pub start_point: Option<String>,
    pub profile: Option<String>,
    pub max_tool_calls: Option<u32>,
    pub max_tokens: Option<u32>,
    pub wall_time_secs: Option<u64>,
    pub create_pr: Option<bool>,
    
    /// Target language module identifier (e.g. "python", "cpp", "sql").
    /// If None, the daemon attempts automatic detection from repo files.
    pub language: Option<String>,

    /// Optional language-specific configuration options (e.g. `{"python_version": "3.12"}`).
    pub language_config: Option<serde_json::Value>,
}
```

### 4.2 New Protocol Methods
1. `language.list`: Returns a list of all registered `LanguageMetadata`.
2. `language.info`: Accepts `LanguageInfoParams { language: String }` and returns detailed tool and version configurations.

---

## 5. Executor Integration Flow

The executor loop in `crates/nebula-daemon/src/executor.rs` integrates language modules into its task setup and teardown:

```rust
// 1. Resolve Language Module (explicit or auto-detected)
let lang_module = if let Some(lang_id) = &params.language {
    shared.languages.get(lang_id)
        .ok_or_else(|| DaemonError::InvalidParams(format!("unknown language module: {lang_id}")))?
} else {
    shared.languages.detect(&worktree_root).await
};

// 2. Provision Hermetic Environment
if let Some(ref module) = lang_module {
    let prov_ctx = ProvisionContext {
        task_id: &task_id.to_string(),
        worktree_root: &worktree_root,
        options: params.language_config.clone(),
        job_handle: Some(task_job.raw_handle()),
    };
    module.provision(&prov_ctx).await?;
}

// 3. Mount Language Tools into ToolHost for this task
// Language tools are scoped directly to the task execution
let mut active_descriptors = shared.tool_host.list_tools();
if let Some(ref module) = lang_module {
    active_descriptors.extend(module.tools());
}

// 4. Inject Specialized System Prompt
let mut system_prompt = String::from("You are Nebula, an autonomous local AI coding assistant...\n");
if let Some(ref module) = lang_module {
    let prov_ctx = ProvisionContext {
        task_id: &task_id.to_string(),
        worktree_root: &worktree_root,
        options: params.language_config.clone(),
        job_handle: Some(task_job.raw_handle()),
    };
    system_prompt.push_str("\n--- Language Module Context ---\n");
    system_prompt.push_str(&module.prompt_guidelines(&prov_ctx));
}

// 5. Execute Loop
// Model calls tools from active_descriptors...

// 6. Finalization
if status == TaskStatus::Completed {
    if let Some(ref module) = lang_module {
        let prov_ctx = ProvisionContext {
            task_id: &task_id.to_string(),
            worktree_root: &worktree_root,
            options: params.language_config.clone(),
            job_handle: Some(task_job.raw_handle()),
        };
        let report = module.finalize(&prov_ctx).await?;
        tracing::info!(event = "language.finalize", summary = %report.summary);
    }
}
```

---

## 6. Telemetry & Observability Events

Language module events are emitted to structured logs:
- `language.detected`: Emitted when auto-detection scores a repository (`repo`, `module`, `confidence`).
- `language.provisioned`: Emitted when environment initialization completes (`module`, `duration_ms`, `venv_path`).
- `language.tool_called`: Emitted when a module-specific tool runs (`module`, `tool`, `duration_ms`, `is_error`).
- `language.finalized`: Emitted when task verification wraps up (`module`, `success`, `artifacts_count`).
