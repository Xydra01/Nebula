//! Generic / polyglot fallback language module.

use std::path::Path;
use std::sync::Arc;

use nebula_proto::LanguageMetadata;
use nebula_tools::BuiltinTool;

use super::{FinalizeReport, LanguageError, LanguageModule, ProvisionContext};

/// Default polyglot language module for general tasks or mixed repositories.
#[derive(Debug, Default)]
pub struct PolyglotModule;

#[async_trait::async_trait]
impl LanguageModule for PolyglotModule {
    fn id(&self) -> &str {
        "polyglot"
    }

    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "polyglot".into(),
            display_name: "Generic / Polyglot".into(),
            description: "Standard environment with basic filesystem, git, and shell tools".into(),
            supported_versions: Vec::new(),
            default_version: None,
        }
    }

    async fn detect(&self, _repo_root: &Path) -> Option<f32> {
        // Baseline fallback confidence
        Some(0.1)
    }

    async fn provision(&self, _ctx: &ProvisionContext<'_>) -> Result<(), LanguageError> {
        Ok(())
    }

    fn tools(&self) -> Vec<Arc<dyn BuiltinTool>> {
        Vec::new()
    }

    fn prompt_guidelines(&self, _ctx: &ProvisionContext<'_>) -> Vec<String> {
        Vec::new()
    }

    async fn finalize(&self, _ctx: &ProvisionContext<'_>) -> Result<FinalizeReport, LanguageError> {
        Ok(FinalizeReport {
            success: true,
            summary: "Polyglot task completed".into(),
            artifacts: Vec::new(),
        })
    }
}
