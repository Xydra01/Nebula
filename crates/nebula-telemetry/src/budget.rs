//! Log disk accounting and the log budget: hot days on `F:`, older days moved to the archive.
//!
//! The log archive is the one exception to the "ask before deleting more than 1 GiB or 500
//! files" rule, since it would otherwise grow without bound: [`enforce_archive_cap`] deletes
//! the oldest archived days on its own. It only ever touches `nebula-YYYY-MM-DD.jsonl` files
//! in the archive directory and always keeps the newest day.
//!
//! Every other prune goes through [`execute_prune`], which refuses a plan above the threshold
//! unless the caller passes [`Confirmation::UserApproved`], which only a human-facing prompt
//! may do.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use time::Date;

use crate::writer::date_of_file_name;

/// Deletes above this many bytes need explicit user approval.
pub const CONFIRM_BYTES: u64 = 1 << 30;
/// Deletes above this many files need explicit user approval.
pub const CONFIRM_FILES: usize = 500;
/// Size cap for the log archive on `D:` (PHASE0_PLAN disk budget).
pub const ARCHIVE_CAP_BYTES: u64 = 8 << 30;

/// Budget failures.
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    /// Filesystem error.
    #[error("log budget I/O at {path}: {source}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
    /// The plan is above the delete threshold and was not approved.
    #[error("deleting {files} files / {bytes} bytes needs user approval")]
    NeedsConfirmation {
        /// Files in the plan.
        files: usize,
        /// Bytes in the plan.
        bytes: u64,
    },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> BudgetError + '_ {
    move |source| BudgetError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Size of the log directory, split into daily logs and blobs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogUsage {
    /// Bytes in daily JSONL files.
    pub log_bytes: u64,
    /// Number of daily JSONL files.
    pub log_files: usize,
    /// Bytes in the blob store.
    pub blob_bytes: u64,
    /// Number of blobs.
    pub blob_files: usize,
}

impl LogUsage {
    /// Total bytes.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.log_bytes + self.blob_bytes
    }
}

fn dir_size(dir: &Path) -> Result<(u64, usize), BudgetError> {
    let mut total = (0, 0);
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(total),
        Err(e) => return Err(io_err(dir)(e)),
    };
    for entry in entries {
        let entry = entry.map_err(io_err(dir))?;
        let meta = entry.metadata().map_err(io_err(&entry.path()))?;
        if meta.is_dir() {
            let (b, n) = dir_size(&entry.path())?;
            total.0 += b;
            total.1 += n;
        } else {
            total.0 += meta.len();
            total.1 += 1;
        }
    }
    Ok(total)
}

/// Daily log files in `dir` with their dates and sizes, oldest first.
///
/// # Errors
/// Filesystem errors reading the directory.
pub fn daily_logs(dir: &Path) -> Result<Vec<(Date, PathBuf, u64)>, BudgetError> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(io_err(dir)(e)),
    };
    for entry in entries {
        let entry = entry.map_err(io_err(dir))?;
        let name = entry.file_name();
        let Some(date) = name.to_str().and_then(date_of_file_name) else {
            continue;
        };
        let meta = entry.metadata().map_err(io_err(&entry.path()))?;
        if meta.is_file() {
            out.push((date, entry.path(), meta.len()));
        }
    }
    out.sort();
    Ok(out)
}

/// Measures daily logs in `log_dir` and blobs in `log_dir\blobs`.
///
/// # Errors
/// Filesystem errors while walking the directories.
pub fn usage(log_dir: &Path) -> Result<LogUsage, BudgetError> {
    let logs = daily_logs(log_dir)?;
    let (blob_bytes, blob_files) = dir_size(&log_dir.join("blobs"))?;
    Ok(LogUsage {
        log_bytes: logs.iter().map(|(_, _, len)| len).sum(),
        log_files: logs.len(),
        blob_bytes,
        blob_files,
    })
}

/// Outcome of [`archive_old_logs`].
#[derive(Debug, Default)]
pub struct ArchiveReport {
    /// Files moved to the archive.
    pub moved: Vec<PathBuf>,
    /// Bytes moved.
    pub bytes: u64,
    /// Files skipped because a file of the same name already exists in the archive.
    pub conflicts: Vec<PathBuf>,
}

/// Moves daily logs older than `hot_days` before `today` from `log_dir` to `archive_dir`.
///
/// The archive is on another drive, so a move is copy, verify length, then remove the
/// source. A name that already exists in the archive is left alone and reported.
///
/// # Errors
/// Filesystem errors. Files moved before the error stay moved.
#[tracing::instrument(level = "debug", skip_all, fields(?today, hot_days))]
pub fn archive_old_logs(
    log_dir: &Path,
    archive_dir: &Path,
    today: Date,
    hot_days: u16,
) -> Result<ArchiveReport, BudgetError> {
    let cutoff = today - time::Duration::days(i64::from(hot_days));
    let mut report = ArchiveReport::default();
    for (date, path, len) in daily_logs(log_dir)? {
        if date >= cutoff {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let dest = archive_dir.join(name);
        if dest.exists() {
            report.conflicts.push(path);
            continue;
        }
        fs::create_dir_all(archive_dir).map_err(io_err(archive_dir))?;
        move_file(&path, &dest, len)?;
        report.bytes += len;
        report.moved.push(dest);
    }
    Ok(report)
}

fn move_file(src: &Path, dest: &Path, len: u64) -> Result<(), BudgetError> {
    if fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    let tmp = dest.with_extension("jsonl.partial");
    let copied = fs::copy(src, &tmp).map_err(io_err(&tmp))?;
    if copied != len {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(dest)(io::Error::other(format!(
            "copied {copied} of {len} bytes"
        ))));
    }
    fs::rename(&tmp, dest).map_err(io_err(dest))?;
    fs::remove_file(src).map_err(io_err(src))
}

/// Archive files that would be deleted to bring the archive under a size cap.
#[derive(Debug, Default)]
pub struct PrunePlan {
    /// Files to delete, oldest first, with sizes.
    pub files: Vec<(PathBuf, u64)>,
    /// Bytes the plan frees.
    pub bytes: u64,
    /// Archive size before the plan.
    pub archive_bytes: u64,
}

impl PrunePlan {
    /// Whether the plan is above the "ask before deleting" threshold.
    #[must_use]
    pub fn needs_confirmation(&self) -> bool {
        self.bytes > CONFIRM_BYTES || self.files.len() > CONFIRM_FILES
    }
}

/// Plans deleting the oldest archived days until the archive is at most `max_bytes`, never
/// including the newest day. Deletes nothing.
///
/// # Errors
/// Filesystem errors reading the archive.
pub fn plan_archive_prune(archive_dir: &Path, max_bytes: u64) -> Result<PrunePlan, BudgetError> {
    let mut logs = daily_logs(archive_dir)?;
    let archive_bytes: u64 = logs.iter().map(|(_, _, len)| len).sum();
    let mut plan = PrunePlan {
        archive_bytes,
        ..PrunePlan::default()
    };
    logs.pop();
    let mut remaining = archive_bytes;
    for (_, path, len) in logs {
        if remaining <= max_bytes {
            break;
        }
        remaining -= len;
        plan.bytes += len;
        plan.files.push((path, len));
    }
    Ok(plan)
}

/// Whether a human approved a prune.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    /// No approval; only plans under the threshold run.
    None,
    /// A person approved this specific plan at a prompt.
    UserApproved,
}

/// Deletes the files in `plan`. Plans above the threshold need [`Confirmation::UserApproved`].
///
/// # Errors
/// [`BudgetError::NeedsConfirmation`], or filesystem errors (files deleted before the
/// error stay deleted).
#[tracing::instrument(level = "info", skip_all, fields(files = plan.files.len(), bytes = plan.bytes))]
pub fn execute_prune(plan: &PrunePlan, confirmation: Confirmation) -> Result<(), BudgetError> {
    if plan.needs_confirmation() && confirmation != Confirmation::UserApproved {
        return Err(BudgetError::NeedsConfirmation {
            files: plan.files.len(),
            bytes: plan.bytes,
        });
    }
    remove_all(plan)
}

fn remove_all(plan: &PrunePlan) -> Result<(), BudgetError> {
    for (path, _) in &plan.files {
        fs::remove_file(path).map_err(io_err(path))?;
    }
    Ok(())
}

/// Deletes the oldest archived days until the archive is at most `max_bytes` (normally
/// [`ARCHIVE_CAP_BYTES`]), without asking: the archive is exempt from the big-delete rule.
/// Returns what was deleted.
///
/// # Errors
/// Filesystem errors (files deleted before the error stay deleted).
#[tracing::instrument(level = "debug", skip_all, fields(max_bytes))]
pub fn enforce_archive_cap(archive_dir: &Path, max_bytes: u64) -> Result<PrunePlan, BudgetError> {
    let plan = plan_archive_prune(archive_dir, max_bytes)?;
    if plan.files.is_empty() {
        return Ok(plan);
    }
    remove_all(&plan)?;
    tracing::info!(
        event = "logs.archive_pruned",
        files = plan.files.len(),
        bytes = plan.bytes,
        archive_bytes = plan.archive_bytes,
        max_bytes,
    );
    Ok(plan)
}
