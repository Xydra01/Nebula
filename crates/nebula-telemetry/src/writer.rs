//! Daily-rotated JSONL file writer: `<dir>\nebula-YYYY-MM-DD.jsonl` (UTC dates).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use time::{Date, OffsetDateTime, format_description::FormatItem, macros::format_description};

/// File name prefix for daily logs.
pub const LOG_PREFIX: &str = "nebula-";
/// File name suffix for daily logs.
pub const LOG_SUFFIX: &str = ".jsonl";

const DATE_FMT: &[FormatItem<'static>] = format_description!("[year]-[month]-[day]");

/// The log file name for a UTC date.
#[must_use]
pub fn file_name_for(date: Date) -> String {
    let d = date.format(DATE_FMT).unwrap_or_default();
    format!("{LOG_PREFIX}{d}{LOG_SUFFIX}")
}

/// Parses the date out of a daily log file name, or `None` if it isn't one.
#[must_use]
pub fn date_of_file_name(name: &str) -> Option<Date> {
    let d = name.strip_prefix(LOG_PREFIX)?.strip_suffix(LOG_SUFFIX)?;
    Date::parse(d, DATE_FMT).ok()
}

/// Appends lines to the file for the current UTC day, switching files at midnight.
#[derive(Debug)]
pub struct DailyJsonl {
    dir: PathBuf,
    current: Option<(Date, BufWriter<File>)>,
}

impl DailyJsonl {
    /// A writer into `dir`, which is created on first write.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            current: None,
        }
    }

    /// Directory being written to.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes one line (a newline is appended) to the file for `now`'s UTC date and flushes,
    /// so a crash loses at most the event being written.
    ///
    /// # Errors
    /// Filesystem errors opening or writing the file.
    pub fn write_line(&mut self, now: OffsetDateTime, line: &str) -> io::Result<()> {
        let date = now.to_offset(time::UtcOffset::UTC).date();
        let file = match &mut self.current {
            Some((d, f)) if *d == date => f,
            _ => {
                fs::create_dir_all(&self.dir)?;
                let f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.dir.join(file_name_for(date)))?;
                &mut self.current.insert((date, BufWriter::new(f))).1
            }
        };
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()
    }

    /// Flushes the current file, if any.
    ///
    /// # Errors
    /// Filesystem errors while flushing.
    pub fn flush(&mut self) -> io::Result<()> {
        match &mut self.current {
            Some((_, f)) => f.flush(),
            None => Ok(()),
        }
    }
}
