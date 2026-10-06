//! The file built-in tools (`fs.read`, `fs.write`, `fs.list`, `fs.search`), confined to the
//! `Worktree_Root` (Requirement 2).
//!
//! Every tool funnels its path argument through the single [`crate::path::resolve`] routine before
//! it touches the filesystem, so path-escape protection lives in exactly one place (Requirement
//! 7.1). A read-style tool (`fs.read`, `fs.list`, `fs.search`) resolves the target path itself; a
//! create-or-update write (`fs.write`) resolves the **parent** directory of the target so a new
//! file is admitted on the basis of where it would be created (Requirement 2.2). On any
//! [`Resolved::Rejected`](crate::path::Resolved::Rejected) — whether the basis is a worktree escape
//! or the retired drive — the tool maps the rejection to [`ToolError::InvalidArguments`], performs
//! no I/O, and leaves the filesystem unchanged (Requirements 2.7, 2.8, 7.7).
//!
//! Tiers are fixed per tool and independent of any particular path's validity (Requirement 2.10,
//! 2.11): `fs.read`/`fs.list`/`fs.search` are [`Tier::Read`] and `fs.write` is [`Tier::Sandbox`].
//! The output cap for `fs.read` and `fs.search` is applied by the host boundary *after* `call`
//! returns, so these tools return the raw [`ToolOutput`] (Requirement 2.12, 5.7).

use std::path::{Path, PathBuf};

use crate::ToolError;
use crate::builtins::{BuiltinTool, ToolContext, ToolOutput};
use crate::path::{self, Resolved};
use crate::permit::Tier;

/// The server label attached to every [`ToolError`] a file tool produces.
const SERVER: &str = "builtin";

/// Resolve `candidate` (relative to the worktree root) through the single path resolver, mapping a
/// confinement rejection to [`ToolError::InvalidArguments`] for `tool`.
///
/// This is the one place the file tools convert a [`Resolved`] into either a permitted
/// [`PathBuf`] to operate on or the `InvalidArguments` error the boundary surfaces (Requirement
/// 7.7). Both rejection bases (worktree escape and retired drive) map to the same error variant;
/// the resolver has already applied retired-drive precedence (Requirement 2.9).
///
/// # Errors
/// [`ToolError::InvalidArguments`] when the resolver rejects the path, or when canonicalizing an
/// existing component fails (for example a permission error surfaced as an I/O error).
fn resolve_permitted(
    candidate: &Path,
    ctx: &ToolContext,
    tool: &str,
) -> Result<PathBuf, ToolError> {
    let worktree_root = ctx.worktree.worktree_root();
    match path::resolve(candidate, &worktree_root, &ctx.retired_drive) {
        Ok(Resolved::Permitted(resolved)) => Ok(resolved),
        Ok(Resolved::Rejected(reason)) => Err(ToolError::InvalidArguments {
            tool: tool.to_owned(),
            detail: format!("path {} rejected: {reason:?}", candidate.display()),
        }),
        Err(err) => Err(ToolError::InvalidArguments {
            tool: tool.to_owned(),
            detail: format!("could not resolve path {}: {err}", candidate.display()),
        }),
    }
}

/// Extract a required, non-empty string field from the already-schema-validated arguments.
///
/// The host validates arguments against [`BuiltinTool::input_schema`] before dispatch, so this is
/// a defensive re-check rather than the primary gate; it still returns a precise
/// [`ToolError::InvalidArguments`] rather than panicking if the shape is somehow off.
fn string_arg(arguments: &serde_json::Value, field: &str, tool: &str) -> Result<String, ToolError> {
    match arguments.get(field).and_then(serde_json::Value::as_str) {
        Some(value) => Ok(value.to_owned()),
        None => Err(ToolError::InvalidArguments {
            tool: tool.to_owned(),
            detail: format!("missing or non-string {field:?} argument"),
        }),
    }
}

/// `fs.read` — return the contents of a file inside the worktree (Requirement 2.3). Tier 0.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsRead;

impl FsRead {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "fs.read";
}

#[async_trait::async_trait]
impl BuiltinTool for FsRead {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some("Read the contents of a file inside the worktree.")
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the worktree root."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn tier(&self) -> Tier {
        Tier::Read
    }

    /// Resolve `path` through the resolver, then return the file's bytes as output.
    ///
    /// # Errors
    /// [`ToolError::InvalidArguments`] when the path is rejected or missing;
    /// [`ToolError::Unavailable`] when the file cannot be read (missing, not a file, I/O error).
    async fn call(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = string_arg(&arguments, "path", Self::NAME)?;
        let resolved = resolve_permitted(Path::new(&path), ctx, Self::NAME)?;
        let bytes = tokio::fs::read(&resolved)
            .await
            .map_err(|e| ToolError::Unavailable {
                server: SERVER.to_owned(),
                detail: format!("could not read {}: {e}", resolved.display()),
            })?;
        Ok(ToolOutput {
            bytes,
            is_error: false,
        })
    }
}

/// `fs.write` — create or update a file inside the worktree (Requirement 2.6). Tier 1.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsWrite;

impl FsWrite {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "fs.write";
}

#[async_trait::async_trait]
impl BuiltinTool for FsWrite {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some("Create or update a file inside the worktree.")
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the worktree root."
                },
                "content": {
                    "type": "string",
                    "description": "The full new contents of the file."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    /// Fixed at [`Tier::Sandbox`] (Tier 1) at registration time, independent of whether any
    /// specific path argument is valid (Requirement 2.11).
    fn tier(&self) -> Tier {
        Tier::Sandbox
    }

    /// Resolve the **parent** directory of `path` through the resolver, then create any missing
    /// parent directories within the permitted destination and create or truncate the file and
    /// write `content`.
    ///
    /// Resolving the parent (rather than the not-yet-existing file) admits a new file on the basis
    /// of where it would be created (Requirement 2.2). The resolver is consulted before anything on
    /// the filesystem is touched, so a rejected path never creates a directory, file, or truncation
    /// (Requirement 2.7, 2.8). Only once the destination's parent is known to be permitted does the
    /// tool materialize any missing parent directories (confined to that permitted parent) so a
    /// nested in-tree write such as `sub/new.txt` succeeds.
    ///
    /// # Errors
    /// [`ToolError::InvalidArguments`] when the parent is rejected, missing, or the path has no
    /// parent (e.g. a bare root); [`ToolError::Unavailable`] when creating the parent directories
    /// or the write itself fails.
    async fn call(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = string_arg(&arguments, "path", Self::NAME)?;
        let content = string_arg(&arguments, "content", Self::NAME)?;
        let target = Path::new(&path);

        // Resolve the PARENT so a new file is admitted by where it would be created (Req 2.2).
        let parent = target.parent().ok_or_else(|| ToolError::InvalidArguments {
            tool: Self::NAME.to_owned(),
            detail: format!("path {path} has no parent directory"),
        })?;
        // An empty parent (e.g. a bare file name) means the worktree root itself.
        let parent_candidate = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let resolved_parent = resolve_permitted(parent_candidate, ctx, Self::NAME)?;

        // Re-attach the final component to the permitted parent to form the real target path.
        let file_name = target
            .file_name()
            .ok_or_else(|| ToolError::InvalidArguments {
                tool: Self::NAME.to_owned(),
                detail: format!("path {path} has no file name"),
            })?;
        let destination = resolved_parent.join(file_name);

        // The resolved parent is permitted but may not yet exist on disk (the resolver admits a
        // not-yet-created directory via its nearest existing ancestor). Create it — and any missing
        // intermediate directories — only now, after confinement has been established, so a nested
        // in-tree write (e.g. `sub/new.txt`) succeeds while a rejected path has already returned
        // above without creating anything (Req 2.2, 2.6, 2.7, 2.8).
        if let Some(parent_dir) = destination.parent() {
            tokio::fs::create_dir_all(parent_dir)
                .await
                .map_err(|e| ToolError::Unavailable {
                    server: SERVER.to_owned(),
                    detail: format!(
                        "could not create parent directories of {}: {e}",
                        destination.display()
                    ),
                })?;
        }

        tokio::fs::write(&destination, content.as_bytes())
            .await
            .map_err(|e| ToolError::Unavailable {
                server: SERVER.to_owned(),
                detail: format!("could not write {}: {e}", destination.display()),
            })?;

        Ok(ToolOutput::text(format!(
            "wrote {} bytes to {}",
            content.len(),
            destination.display()
        )))
    }
}

/// `fs.list` — list the entries of a directory inside the worktree (Requirement 2.4). Tier 0.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsList;

impl FsList {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "fs.list";
}

#[async_trait::async_trait]
impl BuiltinTool for FsList {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some("List the entries of a directory inside the worktree.")
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory, relative to the worktree root."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn tier(&self) -> Tier {
        Tier::Read
    }

    /// Resolve `path` through the resolver, then return the directory's entries as JSON (each entry
    /// carries its name and kind).
    ///
    /// # Errors
    /// [`ToolError::InvalidArguments`] when the path is rejected or missing;
    /// [`ToolError::Unavailable`] when the directory cannot be read.
    async fn call(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = string_arg(&arguments, "path", Self::NAME)?;
        let resolved = resolve_permitted(Path::new(&path), ctx, Self::NAME)?;

        let mut read_dir =
            tokio::fs::read_dir(&resolved)
                .await
                .map_err(|e| ToolError::Unavailable {
                    server: SERVER.to_owned(),
                    detail: format!("could not list {}: {e}", resolved.display()),
                })?;

        let mut entries: Vec<serde_json::Value> = Vec::new();
        loop {
            let next = read_dir
                .next_entry()
                .await
                .map_err(|e| ToolError::Unavailable {
                    server: SERVER.to_owned(),
                    detail: format!("error reading entries of {}: {e}", resolved.display()),
                })?;
            let Some(entry) = next else { break };
            let name = entry.file_name().to_string_lossy().into_owned();
            // A failed file_type is reported as "unknown" rather than failing the whole listing.
            let kind = match entry.file_type().await {
                Ok(ft) if ft.is_dir() => "dir",
                Ok(ft) if ft.is_file() => "file",
                Ok(ft) if ft.is_symlink() => "symlink",
                Ok(_) => "other",
                Err(_) => "unknown",
            };
            entries.push(serde_json::json!({ "name": name, "kind": kind }));
        }
        // Stable ordering so output is deterministic across filesystems.
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

        ToolOutput::json(&serde_json::Value::Array(entries))
    }
}

/// `fs.search` — return literal-substring matches found within a permitted path (Requirement 2.5).
/// Tier 0.
///
/// The search is confined to the resolved path: a file is searched directly; a directory is walked
/// recursively, resolving each descendant through the resolver so a symlink or junction that points
/// outside the worktree (or onto the retired drive) is skipped rather than followed out of the
/// sandbox (Requirement 2.7, 2.8).
#[derive(Clone, Copy, Debug, Default)]
pub struct FsSearch;

impl FsSearch {
    /// The stable tool name used in `tools.list` and `tools.call`.
    pub const NAME: &'static str = "fs.search";
}

#[async_trait::async_trait]
impl BuiltinTool for FsSearch {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> Option<&str> {
        Some("Search for a literal substring in files inside the worktree.")
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File or directory to search, relative to the worktree root."
                },
                "query": {
                    "type": "string",
                    "description": "Literal substring to search for.",
                    "minLength": 1
                }
            },
            "required": ["path", "query"],
            "additionalProperties": false
        })
    }

    fn tier(&self) -> Tier {
        Tier::Read
    }

    /// Resolve `path`, then return every `{ path, line, text }` match for the literal `query`.
    ///
    /// # Errors
    /// [`ToolError::InvalidArguments`] when the path or query is rejected or missing;
    /// [`ToolError::Unavailable`] when the target cannot be read.
    async fn call(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = string_arg(&arguments, "path", Self::NAME)?;
        let query = string_arg(&arguments, "query", Self::NAME)?;
        if query.is_empty() {
            return Err(ToolError::InvalidArguments {
                tool: Self::NAME.to_owned(),
                detail: "query must be non-empty".to_owned(),
            });
        }
        let worktree_root = ctx.worktree.worktree_root();
        let resolved = resolve_permitted(Path::new(&path), ctx, Self::NAME)?;

        let mut matches: Vec<serde_json::Value> = Vec::new();
        let mut stack: Vec<PathBuf> = vec![resolved];
        while let Some(current) = stack.pop() {
            // A path that vanished mid-walk is skipped rather than failing the whole search.
            let Ok(metadata) = tokio::fs::symlink_metadata(&current).await else {
                continue;
            };

            if metadata.is_dir() {
                let Ok(mut read_dir) = tokio::fs::read_dir(&current).await else {
                    continue;
                };
                while let Ok(Some(entry)) = read_dir.next_entry().await {
                    let child = entry.path();
                    // Re-confine every descendant: a symlink/junction target outside the worktree
                    // or on the retired drive is rejected and skipped (Req 2.7, 2.8).
                    if let Ok(Resolved::Permitted(p)) =
                        path::resolve(&child, &worktree_root, &ctx.retired_drive)
                    {
                        stack.push(p);
                    }
                }
            } else if metadata.is_file() {
                let Ok(bytes) = tokio::fs::read(&current).await else {
                    continue;
                };
                // Only search valid UTF-8 content; binary files are skipped.
                let Ok(text) = String::from_utf8(bytes) else {
                    continue;
                };
                let display = current.display().to_string();
                for (index, line) in text.lines().enumerate() {
                    if line.contains(&query) {
                        matches.push(serde_json::json!({
                            "path": display,
                            "line": index + 1,
                            "text": line,
                        }));
                    }
                }
            }
        }

        matches.sort_by(|a, b| {
            let pa = a["path"].as_str().unwrap_or_default();
            let pb = b["path"].as_str().unwrap_or_default();
            let la = a["line"].as_u64().unwrap_or_default();
            let lb = b["line"].as_u64().unwrap_or_default();
            pa.cmp(pb).then(la.cmp(&lb))
        });

        ToolOutput::json(&serde_json::Value::Array(matches))
    }
}

#[cfg(all(test, windows))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::builtins::{BuiltinLimits, ResourceProvider, WorktreeRootProvider};
    use crate::permit::DefaultClassifier;
    use tempfile::TempDir;

    /// A worktree provider returning a fixed root.
    struct FixedWorktree(PathBuf);

    impl WorktreeRootProvider for FixedWorktree {
        fn worktree_root(&self) -> PathBuf {
            self.0.clone()
        }
    }

    /// A resource provider that is never consulted by the file tools.
    struct NoResources;

    impl ResourceProvider for NoResources {
        fn latest(&self) -> Option<nebula_proto::ResourceSnapshot> {
            None
        }
    }

    /// The drive letter of a path, used to pick a retired drive that differs from the temp drive.
    fn drive_letter(path: &Path) -> char {
        let canon = std::fs::canonicalize(path).expect("canonicalize");
        let s = canon.to_string_lossy();
        let trimmed = s.trim_start_matches(r"\\?\");
        trimmed
            .chars()
            .next()
            .expect("a drive letter")
            .to_ascii_uppercase()
    }

    /// Pick a retired drive guaranteed to differ from the worktree's drive so confinement (not the
    /// retired-drive check) is exercised in the common tests.
    fn non_temp_retired_drive(root: &Path) -> String {
        let letter = drive_letter(root);
        let alt = if letter == 'Q' { 'Z' } else { 'Q' };
        format!("{alt}:")
    }

    fn ctx_for(root: &Path, retired_drive: String) -> ToolContext {
        ToolContext {
            worktree: Arc::new(FixedWorktree(root.to_path_buf())),
            classifier: Arc::new(DefaultClassifier),
            resources: Arc::new(NoResources),
            retired_drive,
            limits: BuiltinLimits {
                call_timeout: Duration::from_secs(30),
                output_cap: 65_536,
            },
        }
    }

    #[test]
    fn metadata_and_tiers_are_fixed() {
        assert_eq!(FsRead.name(), "fs.read");
        assert_eq!(FsRead.tier(), Tier::Read);
        assert_eq!(FsList.name(), "fs.list");
        assert_eq!(FsList.tier(), Tier::Read);
        assert_eq!(FsSearch.name(), "fs.search");
        assert_eq!(FsSearch.tier(), Tier::Read);
        // fs.write is Tier 1 independent of any path's validity (Requirement 2.11).
        assert_eq!(FsWrite.name(), "fs.write");
        assert_eq!(FsWrite.tier(), Tier::Sandbox);

        for schema in [
            FsRead.input_schema(),
            FsWrite.input_schema(),
            FsList.input_schema(),
            FsSearch.input_schema(),
        ] {
            assert_eq!(schema["type"], "object");
            // Non-empty schema: at least one required property.
            assert!(schema["required"].as_array().is_some_and(|r| !r.is_empty()));
        }
    }

    #[tokio::test]
    async fn read_returns_file_contents() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("hello.txt"), b"hi there").expect("seed");
        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));

        let out = FsRead
            .call(serde_json::json!({ "path": "hello.txt" }), &ctx)
            .await
            .expect("read permitted file");
        assert_eq!(out.bytes, b"hi there".to_vec());
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn write_creates_a_new_file() {
        let dir = TempDir::new().expect("tempdir");
        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));

        FsWrite
            .call(
                serde_json::json!({ "path": "sub/new.txt", "content": "payload" }),
                &ctx,
            )
            .await
            .expect("write new file");

        let written = std::fs::read(dir.path().join("sub").join("new.txt")).expect("read back");
        assert_eq!(written, b"payload".to_vec());
    }

    #[tokio::test]
    async fn write_updates_an_existing_file() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("exists.txt"), b"old").expect("seed");
        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));

        FsWrite
            .call(
                serde_json::json!({ "path": "exists.txt", "content": "new" }),
                &ctx,
            )
            .await
            .expect("overwrite existing file");

        let written = std::fs::read(dir.path().join("exists.txt")).expect("read back");
        assert_eq!(written, b"new".to_vec());
    }

    #[tokio::test]
    async fn list_returns_entries() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"a").expect("seed a");
        std::fs::create_dir(dir.path().join("subdir")).expect("seed dir");
        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));

        let out = FsList
            .call(serde_json::json!({ "path": "." }), &ctx)
            .await
            .expect("list permitted dir");
        let parsed: serde_json::Value =
            serde_json::from_slice(&out.bytes).expect("list output is JSON");
        let entries = parsed.as_array().expect("array");
        let names: Vec<&str> = entries.iter().filter_map(|e| e["name"].as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"subdir"));
    }

    #[tokio::test]
    async fn search_finds_matches() {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("f.txt"), "alpha\nneedle here\nbeta").expect("seed");
        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));

        let out = FsSearch
            .call(serde_json::json!({ "path": ".", "query": "needle" }), &ctx)
            .await
            .expect("search permitted dir");
        let parsed: serde_json::Value =
            serde_json::from_slice(&out.bytes).expect("search output is JSON");
        let matches = parsed.as_array().expect("array");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["line"], 2);
        assert_eq!(matches[0]["text"], "needle here");
    }

    #[tokio::test]
    async fn read_rejects_parent_escape_with_invalid_arguments() {
        let dir = TempDir::new().expect("tempdir");
        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));

        let err = FsRead
            .call(serde_json::json!({ "path": r"..\..\secret.txt" }), &ctx)
            .await
            .expect_err("escape must be rejected");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn read_rejects_absolute_outside_worktree() {
        let dir = TempDir::new().expect("tempdir");
        let other = TempDir::new().expect("tempdir2");
        let outside = other.path().join("x.txt");
        std::fs::write(&outside, b"secret").expect("seed outside");
        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));

        let err = FsRead
            .call(
                serde_json::json!({ "path": outside.to_string_lossy() }),
                &ctx,
            )
            .await
            .expect_err("absolute outside path must be rejected");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn read_rejects_retired_drive() {
        let dir = TempDir::new().expect("tempdir");
        // Treat the worktree's own drive as retired: any in-tree read must be rejected (Req 2.8).
        let retired = format!("{}:", drive_letter(dir.path()));
        std::fs::write(dir.path().join("inside.txt"), b"data").expect("seed");
        let ctx = ctx_for(dir.path(), retired);

        let err = FsRead
            .call(serde_json::json!({ "path": "inside.txt" }), &ctx)
            .await
            .expect_err("retired-drive read must be rejected");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn write_to_escaping_path_leaves_filesystem_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let other = TempDir::new().expect("tempdir2");
        let target = other.path().join("escape.txt");
        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));

        let err = FsWrite
            .call(
                serde_json::json!({
                    "path": target.to_string_lossy(),
                    "content": "should not land",
                }),
                &ctx,
            )
            .await
            .expect_err("write outside worktree must be rejected");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        // The file must not have been created (Requirement 2.7).
        assert!(!target.exists(), "rejected write must not create the file");
    }

    // Feature: builtin-tools, Property 1 (tool half): on a Rejected resolution the file tool
    // returns InvalidArguments and performs no filesystem operation.
    //
    // The resolver half of Property 1 (no Permitted path escapes confinement) is proven in
    // path.rs. Here we prove the tool-side obligation: for any adversarial relative candidate that
    // escapes the worktree via `..`, `fs.read` and `fs.write` return InvalidArguments and the
    // filesystem is unchanged (no new file for write, no panic/leak for read).
    // Validates: Requirements 2.2, 2.7, 7.7. See design.md, Property 1.
    mod property_rejected_paths {
        use super::*;
        use proptest::prelude::*;

        /// A relative candidate that climbs out of the worktree with a `..` chain plus a leaf.
        fn escaping_candidate() -> impl Strategy<Value = String> {
            (1usize..5, "[a-z][a-z0-9_]{0,7}").prop_map(|(depth, leaf)| {
                let mut parts: Vec<String> = Vec::new();
                for _ in 0..depth {
                    parts.push("..".to_string());
                }
                parts.push(leaf);
                parts.join("\\")
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn write_to_escaping_path_is_rejected_and_changes_nothing(
                candidate in escaping_candidate(),
            ) {
                let dir = TempDir::new().expect("worktree tempdir");
                let outside = TempDir::new().expect("outside tempdir");
                let retired = non_temp_retired_drive(dir.path());
                let ctx = ctx_for(dir.path(), retired);

                // Snapshot the outside dir's entry count so we can assert nothing was created.
                let before = std::fs::read_dir(outside.path())
                    .expect("read outside")
                    .count();

                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                let result = rt.block_on(FsWrite.call(
                    serde_json::json!({ "path": candidate, "content": "x" }),
                    &ctx,
                ));

                let rejected = matches!(result, Err(ToolError::InvalidArguments { .. }));
                prop_assert!(rejected, "a write outside the worktree must be rejected");

                let after = std::fs::read_dir(outside.path())
                    .expect("read outside")
                    .count();
                prop_assert_eq!(before, after, "a rejected write must create nothing");
            }
        }
    }

    /// Create a directory link named `link` inside the worktree whose target is `target` (which
    /// lives outside the worktree). Tries a directory symlink first (needs the symlink privilege,
    /// usually unavailable without elevation or developer mode), then falls back to an NTFS
    /// junction via `cmd /c mklink /J`, which needs no special privilege. Returns the kind of link
    /// that was created, or `None` if neither could be made (so the test skips rather than passing
    /// falsely). The returned string names the mechanism for the test's skip diagnostic.
    fn make_dir_link_outside(link: &Path, target: &Path) -> Option<&'static str> {
        std::fs::create_dir_all(target).expect("create link target dir");
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return Some("symlink");
        }
        // Fall back to an NTFS junction, which does not require the symlink privilege.
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status();
        match status {
            Ok(s) if s.success() && link.exists() => Some("junction"),
            _ => None,
        }
    }

    /// A symlink inside the worktree whose target file lives outside it must be rejected with
    /// `InvalidArguments` and must leave the outside target unchanged (Requirement 2.7). The link
    /// is a directory link (reliable without elevation via a junction fallback); `fs.read` reads a
    /// file reached *through* that link, so following the link escapes the worktree.
    #[tokio::test]
    async fn read_through_symlink_outside_worktree_is_rejected() {
        let dir = TempDir::new().expect("tempdir");
        let outside = TempDir::new().expect("outside tempdir");
        // Seed a secret file in the outside target directory.
        std::fs::write(outside.path().join("secret.txt"), b"top secret").expect("seed secret");

        let link = dir.path().join("escape_link");
        let Some(kind) = make_dir_link_outside(&link, outside.path()) else {
            eprintln!("skipping: could not create a symlink or junction on this host");
            return;
        };
        eprintln!("read_through_symlink_outside_worktree_is_rejected: using a {kind}");

        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));
        // `escape_link/secret.txt` resolves (via the link) to the outside target → escape.
        let err = FsRead
            .call(
                serde_json::json!({ "path": r"escape_link\secret.txt" }),
                &ctx,
            )
            .await
            .expect_err("reading through a link out of the worktree must be rejected");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));

        // The outside secret is untouched (reads never mutate, but assert the FS is unchanged).
        let still = std::fs::read(outside.path().join("secret.txt")).expect("secret still there");
        assert_eq!(still, b"top secret".to_vec());
    }

    /// A write through a symlink/junction whose target is outside the worktree must be rejected
    /// and must create nothing in the outside target (Requirement 2.7).
    #[tokio::test]
    async fn write_through_symlink_outside_worktree_leaves_filesystem_unchanged() {
        let dir = TempDir::new().expect("tempdir");
        let outside = TempDir::new().expect("outside tempdir");

        let link = dir.path().join("escape_link");
        let Some(kind) = make_dir_link_outside(&link, outside.path()) else {
            eprintln!("skipping: could not create a symlink or junction on this host");
            return;
        };
        eprintln!(
            "write_through_symlink_outside_worktree_leaves_filesystem_unchanged: using a {kind}"
        );

        let before = std::fs::read_dir(outside.path())
            .expect("read outside")
            .count();

        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));
        let err = FsWrite
            .call(
                serde_json::json!({
                    "path": r"escape_link\planted.txt",
                    "content": "should not land",
                }),
                &ctx,
            )
            .await
            .expect_err("writing through a link out of the worktree must be rejected");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));

        let after = std::fs::read_dir(outside.path())
            .expect("read outside")
            .count();
        assert_eq!(
            before, after,
            "a rejected write must create nothing outside"
        );
        assert!(!outside.path().join("planted.txt").exists());
    }

    /// An explicit NTFS junction (created via the Win32 `mklink /J` path) whose target is outside
    /// the worktree must be rejected on access. This asserts the junction case specifically rather
    /// than relying on the symlink preference, by requiring a junction and skipping only if the
    /// junction itself cannot be created.
    #[tokio::test]
    async fn read_through_ntfs_junction_outside_worktree_is_rejected() {
        let dir = TempDir::new().expect("tempdir");
        let outside = TempDir::new().expect("outside tempdir");
        std::fs::write(outside.path().join("secret.txt"), b"junction secret").expect("seed");

        // Create a junction explicitly (no symlink attempt): mklink /J needs no privilege.
        let link = dir.path().join("junction_link");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .status();
        let created = matches!(status, Ok(s) if s.success()) && link.exists();
        if !created {
            eprintln!("skipping: could not create an NTFS junction on this host");
            return;
        }

        let ctx = ctx_for(dir.path(), non_temp_retired_drive(dir.path()));
        let err = FsRead
            .call(
                serde_json::json!({ "path": r"junction_link\secret.txt" }),
                &ctx,
            )
            .await
            .expect_err("reading through a junction out of the worktree must be rejected");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));

        let still = std::fs::read(outside.path().join("secret.txt")).expect("secret still there");
        assert_eq!(still, b"junction secret".to_vec());
    }

    /// A literal `C:\...` absolute path must be rejected when `C:` is the configured retired drive
    /// (Requirement 2.8, 7.6), independent of the worktree's own drive. The resolver applies the
    /// retired-drive check before confinement, so the rejection stands even though the path is
    /// also outside the worktree. No real `C:` access occurs: the resolver rejects on the
    /// drive-letter comparison before any I/O against the path.
    #[tokio::test]
    async fn read_literal_c_drive_path_is_rejected_when_c_is_retired() {
        let dir = TempDir::new().expect("tempdir");
        // Configure `C:` as retired regardless of which drive the tempdir lives on.
        let ctx = ctx_for(dir.path(), "C:".to_owned());

        let err = FsRead
            .call(
                serde_json::json!({ "path": r"C:\Windows\System32\drivers\etc\hosts" }),
                &ctx,
            )
            .await
            .expect_err("a C: path must be rejected when C: is retired");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    /// A write targeting a literal `C:\...` path must be rejected when `C:` is retired, and must
    /// perform no I/O against the retired drive (Requirement 2.8). We target a path under a
    /// non-existent `C:` directory so that even if the drive-letter guard regressed, the write
    /// would fail rather than touching a real system path.
    #[tokio::test]
    async fn write_literal_c_drive_path_is_rejected_when_c_is_retired() {
        let dir = TempDir::new().expect("tempdir");
        let ctx = ctx_for(dir.path(), "C:".to_owned());

        let err = FsWrite
            .call(
                serde_json::json!({
                    "path": r"C:\nebula_test_should_never_exist\planted.txt",
                    "content": "should not land",
                }),
                &ctx,
            )
            .await
            .expect_err("a C: write must be rejected when C: is retired");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        assert!(!Path::new(r"C:\nebula_test_should_never_exist").exists());
    }
}
