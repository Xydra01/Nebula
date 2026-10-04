//! Redaction v0: registered secret values plus common token patterns.

use std::borrow::Cow;
use std::sync::{PoisonError, RwLock};

use regex::Regex;
use serde_json::Value;

/// Replacement text for anything redacted.
pub const MASK: &str = "[REDACTED]";

/// Secrets shorter than this are refused by [`Redactor::register`]: masking every
/// occurrence of a 3-character string would shred the logs without protecting anything.
pub const MIN_SECRET_LEN: usize = 8;

const PATTERNS: &[&str] = &[
    // GitHub classic and app tokens.
    r"\bgh[pousr]_[A-Za-z0-9]{36,}\b",
    // GitHub fine-grained PATs.
    r"\bgithub_pat_[A-Za-z0-9_]{22,}\b",
    // Cursor API keys.
    r"\bcursor_[A-Za-z0-9_\-]{16,}\b",
    // Generic bearer tokens, e.g. an Authorization header that ended up in a payload.
    r"(?i)\bbearer\s+[A-Za-z0-9._~+/=\-]{16,}",
];

/// Masks secrets in strings, JSON values and byte payloads before they are written anywhere.
#[derive(Debug)]
pub struct Redactor {
    patterns: Vec<Regex>,
    secrets: RwLock<Vec<String>>,
}

impl Redactor {
    /// A redactor with the built-in patterns and no registered secrets.
    #[must_use]
    pub fn new() -> Self {
        let patterns = PATTERNS.iter().filter_map(|p| Regex::new(p).ok()).collect();
        Self {
            patterns,
            secrets: RwLock::new(Vec::new()),
        }
    }

    /// Registers a secret value (e.g. a token loaded from Credential Manager) to mask
    /// verbatim. Returns `false` and ignores it if it is shorter than [`MIN_SECRET_LEN`].
    pub fn register(&self, secret: impl Into<String>) -> bool {
        let secret = secret.into();
        if secret.chars().count() < MIN_SECRET_LEN {
            return false;
        }
        let mut secrets = self.secrets.write().unwrap_or_else(PoisonError::into_inner);
        if !secrets.contains(&secret) {
            secrets.push(secret);
            // Longest first, so a secret containing another is masked whole.
            secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        }
        true
    }

    /// Returns `text` with every secret and token pattern replaced by [`MASK`].
    #[must_use]
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        {
            let secrets = self.secrets.read().unwrap_or_else(PoisonError::into_inner);
            for secret in secrets.iter() {
                if out.contains(secret.as_str()) {
                    out = Cow::Owned(out.replace(secret.as_str(), MASK));
                }
            }
        }
        for pattern in &self.patterns {
            if let Cow::Owned(replaced) = pattern.replace_all(&out, MASK) {
                out = Cow::Owned(replaced);
            }
        }
        out
    }

    /// Redacts every string inside a JSON value, including object keys' values in nested
    /// objects and arrays. Keys themselves are left alone.
    pub fn redact_value(&self, value: &mut Value) {
        match value {
            Value::String(s) => {
                if let Cow::Owned(r) = self.redact(s) {
                    *s = r;
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.redact_value(v)),
            Value::Object(map) => map.values_mut().for_each(|v| self.redact_value(v)),
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    /// Redacts a byte payload. Valid UTF-8 is redacted as text; other bytes are redacted
    /// lossily so a secret embedded in mostly-text output is still caught.
    #[must_use]
    pub fn redact_bytes<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        let text = String::from_utf8_lossy(bytes);
        match self.redact(&text) {
            Cow::Borrowed(_) => Cow::Borrowed(bytes),
            Cow::Owned(r) => Cow::Owned(r.into_bytes()),
        }
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}
