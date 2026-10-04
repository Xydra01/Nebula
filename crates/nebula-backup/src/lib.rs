//! Backups of Nebula's state and config (PHASE0_PLAN 7.1).
//!
//! `backup_now` packs `state\` (minus volatile bookkeeping) and the config folder into a
//! `tar.zst` in `paths.backups_local`, uploads it to the encrypted rclone remote, then applies
//! retention to both. Deleting more than `max_delete_files` / `max_delete_mib` in one pass stops
//! unless explicitly allowed. The result is recorded in `state\backup-last.json` for `doctor`.

pub mod archive;
pub mod rclone;
pub mod retention;

use std::path::{Path, PathBuf};

use nebula_config::NebulaConfig;
use serde::{Deserialize, Serialize};
use time::macros::format_description;
use time::{Duration, OffsetDateTime};

pub use archive::{Entry, Manifest, Source};
pub use rclone::{Rclone, RemoteFile};
pub use retention::Policy;

/// Backup failures.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// Local file I/O.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// rclone failed.
    #[error("{0}")]
    Rclone(String),
    /// Retention would delete more than the ask-first limit.
    #[error(
        "retention would delete {files} file(s), {mib} MiB from {place}; \
         re-run with --allow-large-delete to confirm"
    )]
    TooManyDeletes {
        /// Where.
        place: String,
        /// File count.
        files: usize,
        /// Total MiB.
        mib: u64,
    },
    /// No such backup.
    #[error("no backup named {0}")]
    NotFound(String),
}

/// State-folder files that change on their own and would defeat change detection.
pub const VOLATILE_STATE: &[&str] = &["doctor.json", "backup-auth.json", RECORD_FILE];

/// The run record in `paths.state`.
pub const RECORD_FILE: &str = "backup-last.json";

const STAMP: &[time::format_description::BorrowedFormatItem<'static>] =
    format_description!("[year][month][day]T[hour][minute][second]Z");

/// `nebula-20261004T170000Z.tar.zst`.
#[must_use]
pub fn archive_name(t: OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!("nebula-{}.tar.zst", t.format(STAMP).unwrap_or_default())
}

/// The time in an archive name, if it is one.
#[must_use]
pub fn parse_name(name: &str) -> Option<OffsetDateTime> {
    let stamp = name.strip_prefix("nebula-")?.strip_suffix(".tar.zst")?;
    time::PrimitiveDateTime::parse(stamp, STAMP)
        .ok()
        .map(time::PrimitiveDateTime::assume_utc)
}

/// What gets backed up: `paths.state` and `paths.config`.
#[must_use]
pub fn sources(cfg: &NebulaConfig) -> Vec<Source> {
    vec![
        Source {
            label: "state".into(),
            dir: cfg.paths.state.clone(),
            exclude: VOLATILE_STATE.iter().map(|s| (*s).to_owned()).collect(),
        },
        Source {
            label: "config".into(),
            dir: cfg.paths.config.clone(),
            exclude: Vec::new(),
        },
    ]
}

/// `state\backup-last.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Last run of any kind.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_run_at: Option<OffsetDateTime>,
    /// Last successful upload.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_uploaded_at: Option<OffsetDateTime>,
    /// Its archive name.
    #[serde(default)]
    pub last_uploaded: Option<String>,
    /// Its content digest, for `--if-changed`.
    #[serde(default)]
    pub last_uploaded_digest: Option<String>,
    /// The last run's error, cleared by a fully successful run.
    #[serde(default)]
    pub last_error: Option<String>,
}

fn record_path(cfg: &NebulaConfig) -> PathBuf {
    cfg.paths.state.join(RECORD_FILE)
}

/// Reads the run record (default if missing or unreadable).
#[must_use]
pub fn load_record(cfg: &NebulaConfig) -> Record {
    std::fs::read_to_string(record_path(cfg))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_record(cfg: &NebulaConfig, r: &Record) -> std::io::Result<()> {
    std::fs::create_dir_all(&cfg.paths.state)?;
    std::fs::write(
        record_path(cfg),
        serde_json::to_vec_pretty(r).map_err(std::io::Error::other)?,
    )
}

/// `backup_now` options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Skip when nothing changed since the last upload (the 6-hourly runs).
    pub if_changed: bool,
    /// Allow a retention pass above the ask-first limit.
    pub allow_large_delete: bool,
}

/// A completed backup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Archive name.
    pub name: String,
    /// Files in it.
    pub files: usize,
    /// Archive bytes.
    pub bytes: u64,
    /// Cloud backups deleted by retention.
    pub pruned_remote: Vec<String>,
    /// Local copies deleted.
    pub pruned_local: Vec<String>,
}

/// What `backup_now` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `if_changed` and nothing changed.
    Unchanged,
    /// Backed up.
    Done(Report),
}

/// Archives older than the newest kept per the policy; `(name, size)`.
#[must_use]
pub fn prune_remote_plan(files: &[RemoteFile], policy: Policy) -> Vec<RemoteFile> {
    let times: Vec<OffsetDateTime> = files.iter().filter_map(|f| parse_name(&f.name)).collect();
    let keep = retention::keep(&times, policy);
    files
        .iter()
        .filter(|f| parse_name(&f.name).is_some_and(|t| !keep.contains(&t)))
        .cloned()
        .collect()
}

/// Local archives older than `local_keep_days`, but never the newest one.
#[must_use]
pub fn prune_local_plan(
    files: &[RemoteFile],
    now: OffsetDateTime,
    keep_days: u64,
) -> Vec<RemoteFile> {
    let cutoff = now - time::Duration::days(i64::try_from(keep_days).unwrap_or(i64::MAX / 86_400));
    let newest = files.iter().filter_map(|f| parse_name(&f.name)).max();
    files
        .iter()
        .filter(|f| parse_name(&f.name).is_some_and(|t| t < cutoff && Some(t) != newest))
        .cloned()
        .collect()
}

fn check_limit(
    cfg: &NebulaConfig,
    place: &str,
    doomed: &[RemoteFile],
    allow: bool,
) -> Result<(), BackupError> {
    let bytes: u64 = doomed.iter().map(|f| f.size).sum();
    let mib = bytes.div_ceil(1024 * 1024);
    if !allow && (doomed.len() > cfg.backup.max_delete_files || mib > cfg.backup.max_delete_mib) {
        return Err(BackupError::TooManyDeletes {
            place: place.to_owned(),
            files: doomed.len(),
            mib,
        });
    }
    Ok(())
}

/// Archives in `paths.backups_local`, oldest first.
///
/// # Errors
/// The folder can't be read (a missing folder is empty).
pub fn local_backups(cfg: &NebulaConfig) -> Result<Vec<RemoteFile>, BackupError> {
    let dir = &cfg.paths.backups_local;
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for e in rd {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if parse_name(&name).is_some() {
            out.push(RemoteFile {
                name,
                size: e.metadata()?.len(),
            });
        }
    }
    out.sort_by_key(|f| parse_name(&f.name));
    Ok(out)
}

/// How old a `.partial` file must be before it counts as abandoned. Longer than the scheduled
/// task's time limit, so a backup or restore still writing one is left alone.
pub const PARTIAL_MAX_AGE: Duration = Duration::hours(1);

/// Half-written archives in `paths.backups_local` (`<archive name>.partial`) last modified
/// before `now - PARTIAL_MAX_AGE`, left behind by a run that was cut off.
///
/// # Errors
/// The folder can't be read (a missing folder is empty).
pub fn stale_partials(
    cfg: &NebulaConfig,
    now: OffsetDateTime,
) -> Result<Vec<RemoteFile>, BackupError> {
    let dir = &cfg.paths.backups_local;
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let cutoff = std::time::SystemTime::from(now - PARTIAL_MAX_AGE);
    let mut out = Vec::new();
    for e in rd {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".partial") else {
            continue;
        };
        let meta = e.metadata()?;
        if parse_name(stem).is_some() && meta.is_file() && meta.modified()? < cutoff {
            out.push(RemoteFile {
                name,
                size: meta.len(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Archives on the remote, oldest first.
///
/// # Errors
/// rclone fails.
pub fn remote_backups(cfg: &NebulaConfig, rclone: &Rclone) -> Result<Vec<RemoteFile>, BackupError> {
    let mut files: Vec<RemoteFile> = rclone
        .list(&cfg.backup.remote)?
        .into_iter()
        .filter(|f| parse_name(&f.name).is_some())
        .collect();
    files.sort_by_key(|f| parse_name(&f.name));
    Ok(files)
}

fn run(
    cfg: &NebulaConfig,
    rclone: &Rclone,
    opts: Options,
    now: OffsetDateTime,
    rec: &mut Record,
) -> Result<Outcome, BackupError> {
    let sources = sources(cfg);
    let entries = archive::collect(&sources)?;
    let digest = archive::digest(&entries);
    if opts.if_changed && rec.last_uploaded_digest.as_deref() == Some(digest.as_str()) {
        return Ok(Outcome::Unchanged);
    }

    let dir = &cfg.paths.backups_local;
    std::fs::create_dir_all(dir)?;
    let name = archive_name(now);
    let partial = dir.join(format!("{name}.partial"));
    let bytes = archive::write(&sources, &entries, now, &partial)?;
    let local = dir.join(&name);
    std::fs::rename(&partial, &local)?;

    rclone.upload(&local, &format!("{}{name}", cfg.backup.remote))?;
    rec.last_uploaded_at = Some(now);
    rec.last_uploaded = Some(name.clone());
    rec.last_uploaded_digest = Some(digest);

    let remote = remote_backups(cfg, rclone)?;
    let doomed = prune_remote_plan(&remote, Policy::from_config(&cfg.backup));
    check_limit(cfg, &cfg.backup.remote, &doomed, opts.allow_large_delete)?;
    for f in &doomed {
        rclone.delete(&format!("{}{}", cfg.backup.remote, f.name))?;
    }

    let locals = local_backups(cfg)?;
    let mut old = prune_local_plan(&locals, now, cfg.backup.local_keep_days);
    old.extend(stale_partials(cfg, now)?);
    check_limit(
        cfg,
        &dir.display().to_string(),
        &old,
        opts.allow_large_delete,
    )?;
    for f in &old {
        std::fs::remove_file(dir.join(&f.name))?;
    }

    Ok(Outcome::Done(Report {
        name,
        files: entries.len(),
        bytes,
        pruned_remote: doomed.into_iter().map(|f| f.name).collect(),
        pruned_local: old.into_iter().map(|f| f.name).collect(),
    }))
}

/// Takes a backup, uploads it and applies retention; records the result either way.
///
/// # Errors
/// Any step failing. The local copy is kept if the upload fails.
pub fn backup_now(
    cfg: &NebulaConfig,
    rclone: &Rclone,
    opts: Options,
    now: OffsetDateTime,
) -> Result<Outcome, BackupError> {
    let mut rec = load_record(cfg);
    let result = run(cfg, rclone, opts, now, &mut rec);
    rec.last_run_at = Some(now);
    rec.last_error = result.as_ref().err().map(ToString::to_string);
    save_record(cfg, &rec)?;
    result
}

/// `nebula-...tar.zst` from a name with or without the extension.
#[must_use]
pub fn normalize_id(id: &str) -> String {
    if id.ends_with(".tar.zst") {
        id.to_owned()
    } else {
        format!("{id}.tar.zst")
    }
}

/// Where a restore goes unless told otherwise: `<backups_local>\..\restore\<id>`.
#[must_use]
pub fn default_restore_dir(cfg: &NebulaConfig, name: &str) -> PathBuf {
    let stem = name.strip_suffix(".tar.zst").unwrap_or(name);
    cfg.paths
        .backups_local
        .parent()
        .unwrap_or(&cfg.paths.backups_local)
        .join("restore")
        .join(stem)
}

/// Unpacks a backup into `dest` and verifies it. Uses the local copy if there is one,
/// otherwise downloads it (keeping the download as a local copy).
///
/// # Errors
/// Not found, download, I/O or verification failures.
pub fn restore(
    cfg: &NebulaConfig,
    rclone: Option<&Rclone>,
    id: &str,
    dest: &Path,
) -> Result<Manifest, BackupError> {
    let name = normalize_id(id);
    if parse_name(&name).is_none() {
        return Err(BackupError::NotFound(id.to_owned()));
    }
    let local = cfg.paths.backups_local.join(&name);
    if !local.exists() {
        let Some(rclone) = rclone else {
            return Err(BackupError::NotFound(format!(
                "{name} (not on D: and no remote)"
            )));
        };
        if !remote_backups(cfg, rclone)?.iter().any(|f| f.name == name) {
            return Err(BackupError::NotFound(name));
        }
        std::fs::create_dir_all(&cfg.paths.backups_local)?;
        let partial = cfg.paths.backups_local.join(format!("{name}.partial"));
        rclone.download(&format!("{}{name}", cfg.backup.remote), &partial)?;
        std::fs::rename(&partial, &local)?;
    }
    Ok(archive::extract(&local, dest)?)
}

/// Copies a verified restore over the original locations. Files not in the backup are left
/// alone. Returns the files written.
///
/// # Errors
/// I/O failures, or a label the manifest doesn't map.
pub fn apply_in_place(manifest: &Manifest, extracted: &Path) -> Result<Vec<PathBuf>, BackupError> {
    let mut written = Vec::new();
    for f in &manifest.files {
        let (label, rel) = f.path.split_once('/').unwrap_or((f.path.as_str(), ""));
        let Some(root) = manifest.sources.get(label) else {
            return Err(BackupError::NotFound(format!("source for {}", f.path)));
        };
        let target = rel.split('/').fold(root.clone(), |p, part| p.join(part));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(extracted.join(&f.path), &target)?;
        written.push(target);
    }
    Ok(written)
}
