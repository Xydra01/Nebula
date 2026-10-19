//! Explicit task completion tool (`task.finish`).
//!
//! Provides a first-class control tool for models to signal task completion, summarize
//! deliverables, and trigger the worktree git auto-commit sequence without waiting
//! for the maximum tool-call limit.

use serde_json::json;

use crate::permit::Tier;
use crate::{BuiltinTool, ToolContext, ToolError, ToolOutput};

/// Explicit task completion tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskFinish;

#[async_trait::async_trait]
impl BuiltinTool for TaskFinish {
    fn name(&self) -> &str {
        "task.finish"
    }

    fn description(&self) -> Option<&str> {
        Some(
            "Signals task completion with a summary of deliverables and verification evidence. Calling this halts the executor loop and records task completion.",
        )
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "String explaining the deliverable and verification proof"
                },
                "status": {
                    "type": "string",
                    "enum": ["success", "failed"],
                    "description": "Final task outcome status (default: success)"
                }
            },
            "required": ["summary"]
        })
    }

    fn tier(&self) -> Tier {
        Tier::Read
    }

    async fn call(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let summary = arguments
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("Task completed successfully.");
        let status = arguments
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("success");

        Ok(ToolOutput::text(format!(
            "Task marked as {status}: {summary}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_matches_expected_contract() {
        let tool = TaskFinish;
        assert_eq!(tool.name(), "task.finish");
        assert_eq!(tool.tier(), Tier::Read);

        let schema = tool.input_schema();
        let required = schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v == "summary"));
    }
}
