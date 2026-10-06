//! The output cap: truncate oversized tool output at a UTF-8 boundary, append a visible marker,
//! and preserve the full, untruncated output in the blob store (design 5, Requirement 5).
//!
//! This replaces PR #42's reject-on-cap behavior. Previously an external tool whose result
//! exceeded the cap was rejected outright with an error; now [`enforce_cap`] is applied uniformly
//! to **both** in-process built-ins and external MCP tool results, so every over-cap result is
//! truncated and preserved the same way regardless of its source (Requirement 5.7). Output within
//! the cap is returned whole with no blob. Output over the cap has its full bytes written to the
//! blob store first, then the inline result is the truncated-at-a-char-boundary prefix plus
//! [`TRUNCATION_MARKER`], staying within the cap. If the blob write fails, the full output cannot
//! be preserved, so the call fails with [`ToolError::OutputTooLarge`] carrying the UTF-8-safe
//! prefix for diagnostics.

use serde_json::json;

use nebula_telemetry::BlobStore;

use crate::ToolError;
use crate::builtins::ToolOutput;
use crate::types::ToolCallResult;

/// Appended to truncated inline output to signal that truncation occurred and the full output is
/// available in the blob store. The exact bytes are stable and asserted in tests.
pub const TRUNCATION_MARKER: &str = "\n…[nebula: output truncated; full output stored in blob]\n";

/// Apply the output cap, measured in bytes against `cap` (Requirement 5.1).
///
/// - If `output.bytes.len() <= cap`, the complete output is returned inline with no blob stored or
///   referenced (Requirement 5.2); the returned blob ref is `None`.
/// - If `output.bytes.len() > cap`, the full untruncated bytes are written to `blobs` first
///   (Requirement 5.3). The inline text is the output truncated at a UTF-8 character boundary so
///   that `prefix + TRUNCATION_MARKER` stays within `cap` bytes, followed by [`TRUNCATION_MARKER`]
///   (Requirement 5.4). The stored blob's reference is returned for the caller to record on the
///   `tool.call` event (Requirement 5.5).
///
/// # Errors
/// [`ToolError::OutputTooLarge`] if the output exceeds `cap` and either no blob store is available
/// or the blob write fails: the full output cannot be preserved, so no successful result and no
/// blob reference are produced, and the UTF-8-safe truncated prefix is carried for diagnostics
/// (Requirement 5.6).
pub fn enforce_cap(
    tool: &str,
    output: ToolOutput,
    cap: usize,
    blobs: Option<&BlobStore>,
) -> Result<(ToolCallResult, Option<String>), ToolError> {
    let ToolOutput { bytes, is_error } = output;

    if bytes.len() <= cap {
        // Within budget: return the whole output inline, no blob (Req 5.2).
        let text = String::from_utf8_lossy(&bytes);
        return Ok((text_result(&text, is_error), None));
    }

    // Over the cap: compute the UTF-8-safe truncated prefix up front so it is available both for
    // the success path and for the blob-write-failure diagnostic (Req 5.4, 5.6).
    let prefix = truncated_prefix(&bytes, cap);

    // Store the full, untruncated output BEFORE producing any inline output (Req 5.3). If no blob
    // store is available or the write fails, the full output cannot be preserved (Req 5.6).
    let blob_ref = match blobs {
        Some(store) => match store.put(&bytes) {
            Ok(r) => r.to_string(),
            Err(e) => {
                tracing::warn!(event = "blob.write_failed", tool = %tool, error = %e);
                return Err(ToolError::OutputTooLarge {
                    tool: tool.to_owned(),
                    got: bytes.len(),
                    cap,
                    prefix,
                });
            }
        },
        None => {
            return Err(ToolError::OutputTooLarge {
                tool: tool.to_owned(),
                got: bytes.len(),
                cap,
                prefix,
            });
        }
    };

    let inline = format!("{prefix}{TRUNCATION_MARKER}");
    Ok((text_result(&inline, is_error), Some(blob_ref)))
}

/// The largest UTF-8-valid prefix of `bytes` whose length plus [`TRUNCATION_MARKER`] stays within
/// `cap` bytes. Walks back from the budget until the prefix is valid UTF-8, so a multibyte code
/// point is never split (Requirement 5.4).
fn truncated_prefix(bytes: &[u8], cap: usize) -> String {
    // Reserve room for the marker so `prefix + marker` stays within the cap. If the marker alone
    // would not fit, the budget is zero and the prefix is empty.
    let budget = cap.saturating_sub(TRUNCATION_MARKER.len());
    let mut end = budget.min(bytes.len());
    loop {
        match std::str::from_utf8(&bytes[..end]) {
            Ok(s) => return s.to_owned(),
            Err(_) if end == 0 => return String::new(),
            Err(_) => end -= 1,
        }
    }
}

/// Wrap inline text in the MCP content shape external tools already use, so the daemon's wire type
/// is unchanged.
fn text_result(text: &str, is_error: bool) -> ToolCallResult {
    ToolCallResult {
        content: json!([{ "type": "text", "text": text }]),
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_cap_returns_whole_output_with_no_blob() {
        let out = ToolOutput::text("hello");
        let (result, blob) = enforce_cap("fs.read", out, 64, None).expect("under cap must succeed");
        assert!(blob.is_none());
        assert!(!result.is_error);
        let text = result.content[0]["text"].as_str().expect("text block");
        assert_eq!(text, "hello");
    }

    #[test]
    fn over_cap_without_blob_store_errors_with_prefix() {
        let big = "x".repeat(1000);
        let out = ToolOutput::text(big);
        let err = enforce_cap("fs.read", out, 100, None).expect_err("over cap, no store -> error");
        match err {
            ToolError::OutputTooLarge {
                tool,
                got,
                cap,
                prefix,
            } => {
                assert_eq!(tool, "fs.read");
                assert_eq!(got, 1000);
                assert_eq!(cap, 100);
                // Prefix is UTF-8-safe and within the cap.
                assert!(prefix.len() <= cap);
            }
            other => panic!("expected OutputTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn marker_bytes_are_stable() {
        assert_eq!(
            TRUNCATION_MARKER,
            "\n…[nebula: output truncated; full output stored in blob]\n"
        );
    }

    #[test]
    fn truncated_prefix_never_splits_a_code_point() {
        // "€" is three bytes (E2 82 AC). A cap that lands mid-character must walk back.
        let s = "€€€€€€€€€€"; // 30 bytes
        let bytes = s.as_bytes();
        let cap = TRUNCATION_MARKER.len() + 4; // budget of 4 bytes -> one full "€" (3 bytes)
        let prefix = truncated_prefix(bytes, cap);
        assert_eq!(prefix, "€");
        // The whole inline string stays within the cap.
        assert!(prefix.len() + TRUNCATION_MARKER.len() <= cap);
    }

    // --- Issue #27 acceptance: output-over-cap truncation + blob (task 5.3) ---

    use std::str::FromStr;
    use std::sync::Arc;

    use nebula_telemetry::{BlobRef, BlobStore, Redactor};
    use tempfile::TempDir;

    /// A real `nebula-telemetry` `BlobStore` backed by a tempdir, with the tempdir kept alive for
    /// the duration of the test so the on-disk blobs can be retrieved.
    fn temp_blob_store() -> (TempDir, BlobStore) {
        let dir = TempDir::new().expect("create tempdir for blob store");
        let store = BlobStore::new(dir.path().join("blobs"), Arc::new(Redactor::new()));
        (dir, store)
    }

    #[test]
    fn over_cap_truncates_to_utf8_boundary_appends_marker_and_preserves_full_output_in_blob() {
        let (_dir, store) = temp_blob_store();

        // Output just over the cap, built from multibyte code points so the boundary at the cap
        // falls mid-character and the truncation must walk back to a valid UTF-8 boundary.
        // "€" is 3 bytes; 50 of them is 150 bytes.
        let full = "€".repeat(50);
        let full_bytes = full.clone().into_bytes();
        let cap = 100usize;
        assert!(
            full_bytes.len() > cap,
            "test precondition: output exceeds cap"
        );

        let out = ToolOutput::text(full.clone());
        let (result, blob) =
            enforce_cap("fs.read", out, cap, Some(&store)).expect("over cap with store succeeds");

        // The blob ref is recorded (Req 5.5).
        let blob_ref = blob.expect("over-cap path must record a blob ref");

        // The full, untruncated output lands in the blob and round-trips byte-for-byte (Req 5.3).
        let parsed = BlobRef::from_str(&blob_ref).expect("returned blob ref parses");
        let retrieved = store.get(&parsed).expect("stored blob is retrievable");
        assert_eq!(retrieved, full_bytes, "blob holds the full original output");

        // The inline text ends with exactly the TRUNCATION_MARKER bytes (Req 5.4).
        let inline = result.content[0]["text"]
            .as_str()
            .expect("inline text block");
        assert!(
            inline.ends_with(TRUNCATION_MARKER),
            "inline output must end with the exact truncation marker"
        );

        // The truncated prefix (everything before the marker) is a valid UTF-8 boundary within the
        // original output, and prefix + marker stays within the cap budget (Req 5.4).
        let prefix = &inline[..inline.len() - TRUNCATION_MARKER.len()];
        assert!(
            full.starts_with(prefix),
            "the inline prefix is a genuine prefix of the full output"
        );
        // A UTF-8 boundary means the prefix contains only whole code points: since the full output
        // is all 3-byte "€", a boundary prefix has a length divisible by 3.
        assert_eq!(
            prefix.len() % 3,
            0,
            "prefix is cut at a UTF-8 code-point boundary, never mid-character"
        );
        assert!(
            prefix.len() + TRUNCATION_MARKER.len() <= cap,
            "prefix + marker stays within the cap budget"
        );
        assert!(!result.is_error);
    }

    #[test]
    fn under_cap_stores_no_blob() {
        let (_dir, store) = temp_blob_store();

        // Output at/under the cap: even with a working blob store available, nothing is stored and
        // no blob ref is returned (Req 5.2).
        let payload = "within budget";
        let out = ToolOutput::text(payload);
        let cap = payload.len() + 100;

        let (result, blob) =
            enforce_cap("fs.read", out, cap, Some(&store)).expect("under cap with store succeeds");

        assert!(
            blob.is_none(),
            "under-cap output must not store or reference a blob"
        );
        let text = result.content[0]["text"]
            .as_str()
            .expect("inline text block");
        assert_eq!(
            text, payload,
            "under-cap output is returned whole and unmodified"
        );
        assert!(!result.is_error);

        // The blob store directory holds no blobs (nothing was written).
        let blobs_dir = store.root();
        let has_entries = std::fs::read_dir(blobs_dir)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
        assert!(
            !has_entries,
            "no blob should have been written under the cap"
        );
    }

    // ---- Property 3 (task 5.2) -------------------------------------------------------------

    use proptest::prelude::*;

    /// Extract the inline text block produced by [`enforce_cap`] on the success path.
    fn inline_text(result: &ToolCallResult) -> String {
        result.content[0]["text"]
            .as_str()
            .expect("success result carries a text block")
            .to_owned()
    }

    /// A blob store whose `put` is guaranteed to fail: it is rooted at an existing *file*, so the
    /// `create_dir_all(root/ab)` inside `put` cannot create a subdirectory under a non-directory.
    /// `enforce_cap` takes the concrete `&BlobStore` (not a trait), so a trait-based fake is not
    /// possible; rooting the real store at a file is the way to drive the write-failure branch.
    fn failing_blob_store() -> (TempDir, BlobStore) {
        let dir = TempDir::new().expect("create tempdir for failing blob store");
        let file_as_root = dir.path().join("not-a-dir");
        std::fs::write(&file_as_root, b"occupied").expect("write a file where the root should be");
        let store = BlobStore::new(file_as_root, Arc::new(Redactor::new()));
        (dir, store)
    }

    /// A payload strategy mixing ASCII and multibyte UTF-8 code points, so generated outputs
    /// routinely straddle a byte cap mid-character (exercising the UTF-8-boundary walk-back). None
    /// of these characters match a redaction pattern, so the stored blob round-trips exactly.
    fn payload_strategy() -> impl Strategy<Value = String> {
        let ch = prop_oneof![
            prop::char::range('a', 'z'), // 1 byte
            Just('é'),                   // 2 bytes
            Just('€'),                   // 3 bytes
            Just('日'),                  // 3 bytes
            Just('🦀'),                  // 4 bytes
        ];
        prop::collection::vec(ch, 0..256).prop_map(|cs| cs.into_iter().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        // Feature: builtin-tools, Property 3: output cap truncates within bounds
        //
        // Validates: Requirements 1.8, 1.9, 2.12, 3.16, 4.10, 5.1, 5.2, 5.3, 5.4, 5.5, 5.6, 5.7
        #[test]
        fn prop_output_cap_truncates_within_bounds_and_preserves_full_output(
            payload in payload_strategy(),
            // Caps at or above the marker length: the design truncates so `prefix + marker` fits
            // the cap, which presumes the marker itself fits. Smaller caps are a degenerate range
            // (empty prefix; marker alone may exceed the cap) outside the operating envelope and
            // are covered by the `truncated_prefix` unit tests.
            cap in TRUNCATION_MARKER.len()..4096usize,
            is_error in any::<bool>(),
        ) {
            let bytes = payload.into_bytes();
            let len = bytes.len();

            if len <= cap {
                // --- Under cap: whole output inline, no blob, even with a working store. ---
                let out = ToolOutput { bytes: bytes.clone(), is_error };
                let (_dir, store) = temp_blob_store();
                let (result, blob) =
                    enforce_cap("fs.read", out, cap, Some(&store)).expect("under cap must succeed");
                prop_assert!(blob.is_none(), "under cap must not store a blob");
                prop_assert_eq!(result.is_error, is_error);
                let text = inline_text(&result);
                // The whole output is returned verbatim (payload is valid UTF-8 by construction).
                prop_assert_eq!(text.as_bytes(), bytes.as_slice());
            } else {
                // --- Over cap with a working store: truncate + marker + preserved blob. ---
                {
                    let out = ToolOutput { bytes: bytes.clone(), is_error };
                    let (_dir, store) = temp_blob_store();
                    let (result, blob_ref) = enforce_cap("fs.read", out, cap, Some(&store))
                        .expect("over cap with a working store must succeed");

                    // A blob reference is returned for logging (Req 5.5).
                    let blob_ref = blob_ref.expect("over cap must return a blob reference");
                    prop_assert_eq!(result.is_error, is_error);

                    let text = inline_text(&result);
                    // Inline is valid UTF-8 (returned as a &str) and within the cap (Req 5.1, 5.4).
                    prop_assert!(
                        text.len() <= cap,
                        "inline {} bytes must be within cap {}",
                        text.len(),
                        cap
                    );
                    // Ends with the exact truncation marker (Req 5.4).
                    prop_assert!(
                        text.ends_with(TRUNCATION_MARKER),
                        "inline must end with the truncation marker"
                    );

                    // The stored blob decodes to EXACTLY the original, untruncated bytes (Req 5.3).
                    let parsed = BlobRef::from_str(&blob_ref).expect("blob ref must parse");
                    let stored = store.get(&parsed).expect("stored blob must read back");
                    prop_assert_eq!(stored.as_slice(), bytes.as_slice());
                }

                // --- Over cap with a failing store: error with UTF-8-safe prefix, no blob. ---
                {
                    let out = ToolOutput { bytes: bytes.clone(), is_error };
                    let (_dir, store) = failing_blob_store();
                    let err = enforce_cap("fs.read", out, cap, Some(&store))
                        .expect_err("over cap with a failing store must error");
                    match err {
                        ToolError::OutputTooLarge { tool, got, cap: errcap, prefix } => {
                            prop_assert_eq!(tool, "fs.read");
                            prop_assert_eq!(got, len);
                            prop_assert_eq!(errcap, cap);
                            // Prefix is UTF-8-safe (a String) and within the cap (Req 5.6).
                            prop_assert!(
                                prefix.len() <= cap,
                                "diagnostic prefix {} bytes must be within cap {}",
                                prefix.len(),
                                cap
                            );
                        }
                        other => prop_assert!(false, "expected OutputTooLarge, got {:?}", other),
                    }
                }

                // --- Over cap with no store at all: same error branch, no blob (Req 5.6). ---
                {
                    let out = ToolOutput { bytes: bytes.clone(), is_error };
                    let err = enforce_cap("fs.read", out, cap, None)
                        .expect_err("over cap with no store must error");
                    let is_too_large = matches!(err, ToolError::OutputTooLarge { .. });
                    prop_assert!(is_too_large, "no-store over-cap must be OutputTooLarge");
                }
            }
        }
    }
}
