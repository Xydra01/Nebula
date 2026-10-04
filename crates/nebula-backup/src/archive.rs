//! Building and unpacking a backup: a `tar.zst` whose first entry, `MANIFEST.json`, lists
//! every file with its size and SHA-256 so a restore can be verified.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// Name of the manifest entry.
pub const MANIFEST: &str = "MANIFEST.json";

/// A directory to back up, stored under `label/` in the archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Top-level folder in the archive, e.g. `state`.
    pub label: String,
    /// Where it lives.
    pub dir: PathBuf,
    /// File names (anywhere below `dir`) to leave out: volatile bookkeeping.
    pub exclude: Vec<String>,
}

/// One file in a backup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// `label/relative/path`, with `/`.
    pub path: String,
    /// Bytes.
    pub size: u64,
    /// Hex SHA-256.
    pub sha256: String,
    /// Absolute source path (not serialized).
    #[serde(skip)]
    pub source: PathBuf,
}

/// `MANIFEST.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Archive creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Label to original directory, for in-place restores.
    pub sources: BTreeMap<String, PathBuf>,
    /// Files.
    pub files: Vec<Entry>,
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = BufReader::new(File::open(path)?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

fn walk(dir: &Path, rel: &str, src: &Source, out: &mut Vec<Entry>) -> io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let child = format!("{rel}/{name}");
        let ty = e.file_type()?;
        if ty.is_dir() {
            walk(&path, &child, src, out)?;
        } else if ty.is_file() && !src.exclude.iter().any(|x| x == &name) {
            out.push(Entry {
                path: child,
                size: e.metadata()?.len(),
                sha256: sha256_file(&path)?,
                source: path,
            });
        }
    }
    Ok(())
}

/// Every file under the sources, in a stable order. Missing source directories are skipped.
///
/// # Errors
/// Reading a directory or file fails.
pub fn collect(sources: &[Source]) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for s in sources {
        if s.dir.is_dir() {
            walk(&s.dir, &s.label, s, &mut out)?;
        }
    }
    Ok(out)
}

/// A digest of paths, sizes and contents: equal digests mean nothing changed.
#[must_use]
pub fn digest(entries: &[Entry]) -> String {
    let mut h = Sha256::new();
    for e in entries {
        h.update(e.path.as_bytes());
        h.update([0]);
        h.update(e.size.to_le_bytes());
        h.update(e.sha256.as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

/// Writes the archive to `dest`. Returns its size.
///
/// # Errors
/// I/O failures; a file that changed size while being read.
pub fn write(
    sources: &[Source],
    entries: &[Entry],
    created_at: OffsetDateTime,
    dest: &Path,
) -> io::Result<u64> {
    let manifest = Manifest {
        created_at,
        sources: sources
            .iter()
            .map(|s| (s.label.clone(), s.dir.clone()))
            .collect(),
        files: entries.to_vec(),
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    let file = BufWriter::new(File::create(dest)?);
    let enc = zstd::Encoder::new(file, 19)?;
    let mut tar = tar::Builder::new(enc);
    let mut header = tar::Header::new_gnu();
    header.set_size(json.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(u64::try_from(created_at.unix_timestamp()).unwrap_or(0));
    header.set_cksum();
    tar.append_data(&mut header, MANIFEST, json.as_slice())?;
    for e in entries {
        let mut f = File::open(&e.source)?;
        if f.metadata()?.len() != e.size {
            return Err(io::Error::other(format!(
                "{} changed while being backed up",
                e.source.display()
            )));
        }
        tar.append_file(&e.path, &mut f)?;
    }
    let enc = tar.into_inner()?;
    let mut file = enc.finish()?;
    io::Write::flush(&mut file)?;
    drop(file);
    Ok(std::fs::metadata(dest)?.len())
}

/// Unpacks `archive` into `dest` (which must not exist or be empty) and checks every file
/// against the manifest.
///
/// # Errors
/// I/O failures, a missing manifest, or a file that doesn't match it.
pub fn extract(archive: &Path, dest: &Path) -> io::Result<Manifest> {
    if dest.exists() && std::fs::read_dir(dest)?.next().is_some() {
        return Err(io::Error::other(format!("{} is not empty", dest.display())));
    }
    std::fs::create_dir_all(dest)?;
    let dec = zstd::Decoder::new(File::open(archive)?)?;
    tar::Archive::new(dec).unpack(dest)?;
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(dest.join(MANIFEST))?)
        .map_err(|e| io::Error::other(format!("bad {MANIFEST}: {e}")))?;
    for f in &manifest.files {
        let path = dest.join(&f.path);
        let got = sha256_file(&path)?;
        if got != f.sha256 {
            return Err(io::Error::other(format!(
                "{} does not match the manifest",
                f.path
            )));
        }
    }
    Ok(manifest)
}
