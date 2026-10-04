//! Content-addressed blob store for large log payloads (prompts, outputs, stderr).

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::redact::Redactor;

const ZSTD_LEVEL: i32 = 3;
const PREFIX: &str = "sha256:";

/// Reference to a stored blob: `sha256:<64 hex chars>` of the stored (redacted) content.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BlobRef {
    hex: String,
}

impl BlobRef {
    /// The hex digest without the `sha256:` prefix.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }
}

impl fmt::Display for BlobRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{PREFIX}{}", self.hex)
    }
}

impl FromStr for BlobRef {
    type Err = BlobError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix(PREFIX)
            .ok_or_else(|| BlobError::BadRef(s.to_owned()))?;
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(BlobError::BadRef(s.to_owned()));
        }
        Ok(Self {
            hex: hex.to_owned(),
        })
    }
}

impl TryFrom<String> for BlobRef {
    type Error = BlobError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<BlobRef> for String {
    fn from(r: BlobRef) -> Self {
        r.to_string()
    }
}

/// Blob store failures.
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    /// Not a `sha256:<hex>` reference.
    #[error("invalid blob reference {0:?}")]
    BadRef(String),
    /// No blob with that reference.
    #[error("blob {0} not found")]
    NotFound(BlobRef),
    /// Stored content does not match its address.
    #[error("blob {0} is corrupt")]
    Corrupt(BlobRef),
    /// Filesystem error.
    #[error("blob store I/O at {path}: {source}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> BlobError + '_ {
    move |source| BlobError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Stores zstd-compressed blobs at `<root>/ab/cdef….zst`, keyed by the SHA-256 of the
/// redacted content, so identical payloads are stored once.
#[derive(Clone, Debug)]
pub struct BlobStore {
    root: PathBuf,
    redactor: Arc<Redactor>,
}

impl BlobStore {
    /// A store rooted at `root` (normally `<log_dir>\blobs`). The directory is created lazily.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, redactor: Arc<Redactor>) -> Self {
        Self {
            root: root.into(),
            redactor,
        }
    }

    /// Root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path where a blob is (or would be) stored.
    #[must_use]
    pub fn path_of(&self, r: &BlobRef) -> PathBuf {
        let (dir, rest) = r.hex.split_at(2);
        self.root.join(dir).join(format!("{rest}.zst"))
    }

    /// Redacts, hashes and stores `bytes`, returning its reference. Storing the same content
    /// again is a no-op that returns the same reference.
    ///
    /// # Errors
    /// Filesystem errors while creating the directory or writing the file.
    #[tracing::instrument(level = "trace", skip_all, fields(len = bytes.len()))]
    pub fn put(&self, bytes: &[u8]) -> Result<BlobRef, BlobError> {
        let content = self.redactor.redact_bytes(bytes);
        let r = BlobRef {
            hex: hex::encode(Sha256::digest(&content)),
        };
        let path = self.path_of(&r);
        if path.exists() {
            return Ok(r);
        }
        let dir = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(dir).map_err(io_err(dir))?;
        let compressed = zstd::encode_all(&content[..], ZSTD_LEVEL).map_err(io_err(&path))?;
        // Write to a unique temp name and rename, so a crash never leaves a truncated blob
        // under its final address and concurrent writers of the same blob don't collide.
        let tmp = dir.join(format!(
            ".{}.{}.tmp",
            &r.hex[2..],
            nebula_proto::SpanId::new()
        ));
        let write = || -> io::Result<()> {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&compressed)?;
            f.sync_all()
        };
        if let Err(e) = write() {
            let _ = fs::remove_file(&tmp);
            return Err(io_err(&tmp)(e));
        }
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            // Another writer won the race with identical content.
            if path.exists() {
                return Ok(r);
            }
            return Err(io_err(&path)(e));
        }
        Ok(r)
    }

    /// Reads and decompresses a blob, verifying its hash.
    ///
    /// # Errors
    /// [`BlobError::NotFound`], [`BlobError::Corrupt`] or I/O errors.
    pub fn get(&self, r: &BlobRef) -> Result<Vec<u8>, BlobError> {
        let path = self.path_of(r);
        let compressed = match fs::read(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(BlobError::NotFound(r.clone()));
            }
            Err(e) => return Err(io_err(&path)(e)),
        };
        let content =
            zstd::decode_all(&compressed[..]).map_err(|_| BlobError::Corrupt(r.clone()))?;
        if hex::encode(Sha256::digest(&content)) != r.hex {
            return Err(BlobError::Corrupt(r.clone()));
        }
        Ok(content)
    }
}
