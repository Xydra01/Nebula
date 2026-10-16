//! Circuit breaker tracker for autonomous executor tasks (issue #34).
//!
//! Halts tasks when repeated identical failures, repeated file edit loops,
//! or runaway token use is detected.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;

use nebula_config::CircuitBreakersConfig;
use serde_json::Value;

/// Why a circuit breaker tripped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BreakerTrip {
    /// The same tool and arguments failed repeatedly.
    RepeatedToolFailure {
        /// Tool that failed.
        tool: String,
        /// Number of consecutive identical failures.
        count: u32,
    },
    /// The same file was modified repeatedly in one task.
    RepeatedFileEdits {
        /// Path of the file.
        path: PathBuf,
        /// Number of modifications.
        count: u32,
    },
    /// A single step consumed an abnormally high number of tokens.
    RunawayTokens {
        /// Tokens consumed in the step.
        tokens: u32,
        /// Configured limit.
        limit: u32,
    },
}

impl std::fmt::Display for BreakerTrip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RepeatedToolFailure { tool, count } => {
                write!(
                    f,
                    "tool '{tool}' failed {count} times consecutively with identical arguments"
                )
            }
            Self::RepeatedFileEdits { path, count } => {
                write!(
                    f,
                    "file '{}' modified {count} times in task",
                    path.display()
                )
            }
            Self::RunawayTokens { tokens, limit } => {
                write!(f, "step token usage ({tokens}) exceeded limit ({limit})")
            }
        }
    }
}

/// Tracks turn-by-turn metrics and trips breakers when thresholds are exceeded.
#[derive(Clone, Debug)]
pub struct CircuitBreakerTracker {
    config: CircuitBreakersConfig,
    consecutive_failures: Option<(String, u64, u32)>,
    file_edit_counts: HashMap<PathBuf, u32>,
}

fn hash_arguments(args: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    let repr = serde_json::to_string(args).unwrap_or_default();
    repr.hash(&mut hasher);
    hasher.finish()
}

fn extract_target_file(tool: &str, args: &Value) -> Option<PathBuf> {
    if tool == "fs.write"
        || tool.ends_with(".write")
        || tool.ends_with(".edit")
        || tool == "edit"
        || tool == "write"
    {
        let raw_path = args.get("path").or_else(|| args.get("file"))?.as_str()?;
        let normalized = raw_path.replace('\\', "/");
        Some(PathBuf::from(normalized))
    } else {
        None
    }
}

impl CircuitBreakerTracker {
    /// Creates a tracker with the given configuration thresholds.
    #[must_use]
    pub fn new(config: CircuitBreakersConfig) -> Self {
        Self {
            config,
            consecutive_failures: None,
            file_edit_counts: HashMap::new(),
        }
    }

    /// Check whether a single step's token consumption exceeds runaway limits.
    ///
    /// # Errors
    /// Returns [`BreakerTrip::RunawayTokens`] if `step_tokens` >= `runaway_step_tokens`.
    pub fn check_step_tokens(&self, step_tokens: u32) -> Result<(), BreakerTrip> {
        if self.config.runaway_step_tokens > 0 && step_tokens >= self.config.runaway_step_tokens {
            return Err(BreakerTrip::RunawayTokens {
                tokens: step_tokens,
                limit: self.config.runaway_step_tokens,
            });
        }
        Ok(())
    }

    /// Record a tool call outcome and check repeated failure and file edit breakers.
    ///
    /// # Errors
    /// Returns [`BreakerTrip::RepeatedToolFailure`] if the same tool failed `max_repeated_tool_failures` times consecutively.
    /// Returns [`BreakerTrip::RepeatedFileEdits`] if the same file was modified `max_file_edits` times.
    pub fn record_tool_call(
        &mut self,
        tool: &str,
        args: &Value,
        is_error: bool,
    ) -> Result<(), BreakerTrip> {
        if is_error {
            let h = hash_arguments(args);
            let next_count = match &self.consecutive_failures {
                Some((prev_tool, prev_hash, count)) if prev_tool == tool && *prev_hash == h => {
                    count + 1
                }
                _ => 1,
            };
            self.consecutive_failures = Some((tool.to_owned(), h, next_count));
            if self.config.max_repeated_tool_failures > 0
                && next_count >= self.config.max_repeated_tool_failures
            {
                return Err(BreakerTrip::RepeatedToolFailure {
                    tool: tool.to_owned(),
                    count: next_count,
                });
            }
        } else {
            // A successful call resets consecutive failure tracking
            self.consecutive_failures = None;
        }

        if let Some(target) = extract_target_file(tool, args) {
            let count = self.file_edit_counts.entry(target.clone()).or_insert(0);
            *count += 1;
            if self.config.max_file_edits > 0 && *count >= self.config.max_file_edits {
                return Err(BreakerTrip::RepeatedFileEdits {
                    path: target,
                    count: *count,
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_config() -> CircuitBreakersConfig {
        CircuitBreakersConfig {
            max_repeated_tool_failures: 3,
            max_file_edits: 5,
            runaway_step_tokens: 16384,
        }
    }

    #[test]
    fn repeated_tool_failure_trips_at_threshold() {
        let mut tracker = CircuitBreakerTracker::new(test_config());
        let args = json!({ "command": "cargo build" });

        assert!(tracker.record_tool_call("shell.run", &args, true).is_ok());
        assert!(tracker.record_tool_call("shell.run", &args, true).is_ok());

        let res = tracker.record_tool_call("shell.run", &args, true);
        assert_eq!(
            res,
            Err(BreakerTrip::RepeatedToolFailure {
                tool: "shell.run".into(),
                count: 3
            })
        );
    }

    #[test]
    fn tool_success_resets_failure_counter() {
        let mut tracker = CircuitBreakerTracker::new(test_config());
        let args = json!({ "command": "cargo test" });

        assert!(tracker.record_tool_call("shell.run", &args, true).is_ok());
        assert!(tracker.record_tool_call("shell.run", &args, true).is_ok());
        // Success resets counter:
        assert!(tracker.record_tool_call("shell.run", &args, false).is_ok());
        // First error again:
        assert!(tracker.record_tool_call("shell.run", &args, true).is_ok());
        assert!(tracker.record_tool_call("shell.run", &args, true).is_ok());
        // Trips on 3rd consecutive after reset:
        assert_eq!(
            tracker.record_tool_call("shell.run", &args, true),
            Err(BreakerTrip::RepeatedToolFailure {
                tool: "shell.run".into(),
                count: 3
            })
        );
    }

    #[test]
    fn different_args_or_tools_track_separately() {
        let mut tracker = CircuitBreakerTracker::new(test_config());
        let args1 = json!({ "command": "cargo test" });
        let args2 = json!({ "command": "cargo clippy" });

        assert!(tracker.record_tool_call("shell.run", &args1, true).is_ok());
        assert!(tracker.record_tool_call("shell.run", &args1, true).is_ok());
        // Different args resets consecutive identical counter:
        assert!(tracker.record_tool_call("shell.run", &args2, true).is_ok());
        assert!(tracker.record_tool_call("shell.run", &args2, true).is_ok());
        assert_eq!(
            tracker.record_tool_call("shell.run", &args2, true),
            Err(BreakerTrip::RepeatedToolFailure {
                tool: "shell.run".into(),
                count: 3
            })
        );
    }

    #[test]
    fn repeated_file_edits_trips_at_threshold() {
        let mut tracker = CircuitBreakerTracker::new(test_config());
        let args = json!({ "path": "src/main.rs", "content": "fn main() {}" });

        for _ in 0..4 {
            assert!(tracker.record_tool_call("fs.write", &args, false).is_ok());
        }

        let res = tracker.record_tool_call("fs.write", &args, false);
        assert_eq!(
            res,
            Err(BreakerTrip::RepeatedFileEdits {
                path: PathBuf::from("src/main.rs"),
                count: 5
            })
        );
    }

    #[test]
    fn different_files_have_separate_edit_counts() {
        let mut tracker = CircuitBreakerTracker::new(test_config());
        let file1 = json!({ "path": "src/lib.rs", "content": "// 1" });
        let file2 = json!({ "path": "src/main.rs", "content": "// 2" });

        for _ in 0..4 {
            assert!(tracker.record_tool_call("fs.write", &file1, false).is_ok());
            assert!(tracker.record_tool_call("fs.write", &file2, false).is_ok());
        }

        // Neither has hit 5 yet:
        assert_eq!(
            tracker.record_tool_call("fs.write", &file1, false),
            Err(BreakerTrip::RepeatedFileEdits {
                path: PathBuf::from("src/lib.rs"),
                count: 5
            })
        );
    }

    #[test]
    fn runaway_tokens_checks() {
        let tracker = CircuitBreakerTracker::new(test_config());

        assert!(tracker.check_step_tokens(1000).is_ok());
        assert!(tracker.check_step_tokens(16383).is_ok());

        assert_eq!(
            tracker.check_step_tokens(16384),
            Err(BreakerTrip::RunawayTokens {
                tokens: 16384,
                limit: 16384
            })
        );
    }
}
