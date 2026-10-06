//! The `system.resources` built-in tool (Requirement 6).
//!
//! `system.resources` wraps the existing resource snapshot so the agent can observe host load
//! through the same tool interface as every other built-in. It is a pure read: it takes no
//! arguments, performs no I/O of its own, and simply returns the most recent
//! [`nebula_proto::ResourceSnapshot`] supplied by the injected [`ResourceProvider`].
//!
//! Because the provider hands back the same `ResourceSnapshot` the daemon's `resources.snapshot`
//! method returns for the same sampler state, the fields the tool emits are equal field-for-field
//! to that method's result (Requirement 6.4). When the sampler has not yet produced a snapshot the
//! tool reports [`ToolError::Unavailable`] rather than fabricating a value (Requirement 6.3).

use crate::ToolError;
use crate::builtins::{BuiltinTool, ToolContext, ToolOutput};
use crate::permit::Tier;

/// The `system.resources` built-in tool.
///
/// Returns the latest [`nebula_proto::ResourceSnapshot`] from [`ToolContext::resources`], or
/// [`ToolError::Unavailable`] when none has been taken yet. Classified at [`Tier::Read`]
/// (Requirement 6.5): it never writes or executes anything.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResources;

impl SystemResources {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "system.resources";
}

#[async_trait::async_trait]
impl BuiltinTool for SystemResources {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some("Return the most recent system resource snapshot (CPU, memory, GPU, disk).")
    }

    /// Empty-object schema: the tool takes no arguments. `additionalProperties: false` rejects any
    /// supplied field at the boundary before [`call`](Self::call) runs.
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn tier(&self) -> Tier {
        Tier::Read
    }

    /// Return the latest resource snapshot as JSON.
    ///
    /// `arguments` have already passed schema validation (an empty object) and are ignored.
    ///
    /// # Errors
    /// [`ToolError::Unavailable`] when the sampler has not yet produced a snapshot
    /// (Requirement 6.3); [`ToolError::Protocol`] if the snapshot cannot be serialized.
    async fn call(
        &self,
        _arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        match ctx.resources.latest() {
            Some(snapshot) => {
                let value = serde_json::to_value(&snapshot).map_err(|e| ToolError::Protocol {
                    server: "builtin".to_owned(),
                    detail: format!("failed to serialize resource snapshot: {e}"),
                })?;
                ToolOutput::json(&value)
            }
            None => Err(ToolError::Unavailable {
                server: "builtin".to_owned(),
                detail: "no resource snapshot yet".to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::builtins::{BuiltinLimits, ResourceProvider, ToolContext, WorktreeRootProvider};
    use crate::permit::DefaultClassifier;

    /// A worktree provider that is never consulted by `system.resources`.
    struct UnusedWorktree;

    impl WorktreeRootProvider for UnusedWorktree {
        fn worktree_root(&self) -> std::path::PathBuf {
            std::path::PathBuf::from(".")
        }
    }

    /// A resource provider returning a fixed `Option<ResourceSnapshot>`.
    struct FixedResources(Option<nebula_proto::ResourceSnapshot>);

    impl ResourceProvider for FixedResources {
        fn latest(&self) -> Option<nebula_proto::ResourceSnapshot> {
            self.0.clone()
        }
    }

    fn ctx_with(resources: Option<nebula_proto::ResourceSnapshot>) -> ToolContext {
        ToolContext {
            worktree: Arc::new(UnusedWorktree),
            classifier: Arc::new(DefaultClassifier),
            resources: Arc::new(FixedResources(resources)),
            retired_drive: "C:".to_owned(),
            limits: BuiltinLimits {
                call_timeout: Duration::from_secs(30),
                output_cap: 65_536,
            },
        }
    }

    fn sample_snapshot() -> nebula_proto::ResourceSnapshot {
        // Built by deserializing JSON so the test avoids a direct `time` crate dependency to
        // construct the `OffsetDateTime` field.
        serde_json::from_value(serde_json::json!({
            "taken_at": "1970-01-01T00:00:00Z",
            "vram_used_mib": 2_048,
            "vram_total_mib": 24_576,
            "gpu_util_pct": 42,
            "ram_used_mib": 8_000,
            "ram_total_mib": 32_000,
            "commit_used_mib": 10_000,
            "commit_limit_mib": 48_000,
            "cpu_pct": 12.5,
            "disks": [],
            "gpu_processes": []
        }))
        .expect("sample snapshot JSON should deserialize")
    }

    #[test]
    fn metadata_is_tier_read_with_empty_schema() {
        let tool = SystemResources;
        assert_eq!(tool.name(), "system.resources");
        assert_eq!(tool.tier(), Tier::Read);
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert!(tool.description().is_some());
    }

    #[tokio::test]
    async fn returns_snapshot_json_when_present() {
        let snapshot = sample_snapshot();
        let ctx = ctx_with(Some(snapshot.clone()));
        let out = SystemResources
            .call(serde_json::json!({}), &ctx)
            .await
            .expect("snapshot present should succeed");
        assert!(!out.is_error);

        // Field-for-field equality with the proto serialization (Requirement 6.4). Compare parsed
        // JSON values rather than raw bytes so object key ordering is not significant.
        let produced: serde_json::Value =
            serde_json::from_slice(&out.bytes).expect("tool output is valid JSON");
        let expected = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert_eq!(produced, expected);
    }

    #[tokio::test]
    async fn unavailable_when_no_snapshot_yet() {
        let ctx = ctx_with(None);
        let err = SystemResources
            .call(serde_json::json!({}), &ctx)
            .await
            .expect_err("missing snapshot should be Unavailable");
        match err {
            ToolError::Unavailable { server, detail } => {
                assert_eq!(server, "builtin");
                assert_eq!(detail, "no resource snapshot yet");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    // ---- Property 8 (task 10.2) ----------------------------------------------------------------

    mod property_round_trip {
        use super::*;
        use nebula_proto::ResourceSnapshot;
        use proptest::prelude::*;

        /// Build a `ResourceSnapshot` by deserializing a generated JSON object. This mirrors the
        /// unit test's construction so the test needs no direct `time` dependency to build the
        /// `OffsetDateTime` field.
        ///
        /// `taken_at` carries whole seconds only, so the RFC3339 text the tool emits round-trips
        /// back to the exact same instant (no sub-second component to drop). `cpu_pct` is finite:
        /// serde_json renders NaN/infinity as `null`, which would not deserialize to the same
        /// `f32`. Every other field is an integer, string, or collection that serializes
        /// losslessly.
        fn snapshot_strategy() -> impl Strategy<Value = ResourceSnapshot> {
            // A valid RFC3339 timestamp assembled from in-range date/time parts.
            let taken_at = (
                1970u32..=2200u32,
                1u32..=12u32,
                1u32..=28u32,
                0u32..=23u32,
                0u32..=59u32,
                0u32..=59u32,
            )
                .prop_map(|(y, mo, d, h, mi, s)| {
                    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
                });

            // Each disk: mount string + two byte counts.
            let disk = (any::<String>(), any::<u64>(), any::<u64>()).prop_map(
                |(mount, free_bytes, total_bytes)| {
                    serde_json::json!({
                        "mount": mount,
                        "free_bytes": free_bytes,
                        "total_bytes": total_bytes,
                    })
                },
            );

            // Each GPU process: pid + name + VRAM.
            let gpu_process =
                (any::<u32>(), any::<String>(), any::<u64>()).prop_map(|(pid, name, vram_mib)| {
                    serde_json::json!({
                        "pid": pid,
                        "name": name,
                        "vram_mib": vram_mib,
                    })
                });

            (
                taken_at,
                any::<u64>(),
                any::<u64>(),
                proptest::option::of(any::<u8>()),
                any::<u64>(),
                any::<u64>(),
                any::<u64>(),
                any::<u64>(),
                any::<f32>().prop_filter("finite cpu_pct", |v| v.is_finite()),
                proptest::collection::vec(disk, 0..4),
                proptest::collection::vec(gpu_process, 0..4),
            )
                .prop_map(
                    |(
                        taken_at,
                        vram_used_mib,
                        vram_total_mib,
                        gpu_util_pct,
                        ram_used_mib,
                        ram_total_mib,
                        commit_used_mib,
                        commit_limit_mib,
                        cpu_pct,
                        disks,
                        gpu_processes,
                    )| {
                        let value = serde_json::json!({
                            "taken_at": taken_at,
                            "vram_used_mib": vram_used_mib,
                            "vram_total_mib": vram_total_mib,
                            "gpu_util_pct": gpu_util_pct,
                            "ram_used_mib": ram_used_mib,
                            "ram_total_mib": ram_total_mib,
                            "commit_used_mib": commit_used_mib,
                            "commit_limit_mib": commit_limit_mib,
                            "cpu_pct": cpu_pct,
                            "disks": disks,
                            "gpu_processes": gpu_processes,
                        });
                        serde_json::from_value(value).expect("generated snapshot JSON deserializes")
                    },
                )
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            // Feature: builtin-tools, Property 8: system.resources output equals the snapshot field-for-field
            //
            // Validates: Requirements 6.2, 6.4
            #[test]
            fn prop_system_resources_output_equals_snapshot(snapshot in snapshot_strategy()) {
                let ctx = ctx_with(Some(snapshot.clone()));
                let out = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("current-thread runtime builds")
                    .block_on(async { SystemResources.call(serde_json::json!({}), &ctx).await })
                    .expect("snapshot present should succeed");
                prop_assert!(!out.is_error);

                // Deserialize the tool's JSON output back into a `ResourceSnapshot` and assert
                // field-for-field equality with the input (`ResourceSnapshot: PartialEq`).
                let round_tripped: ResourceSnapshot = serde_json::from_slice(&out.bytes)
                    .expect("tool output deserializes to ResourceSnapshot");
                prop_assert_eq!(round_tripped, snapshot);
            }
        }
    }
}
