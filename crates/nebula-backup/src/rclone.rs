//! The rclone calls backups need. The config password goes through `RCLONE_CONFIG_PASS`, never
//! the command line.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::BackupError;

/// An rclone binary, config file and password.
#[derive(Clone)]
pub struct Rclone {
    exe: PathBuf,
    config: PathBuf,
    pass: String,
}

impl std::fmt::Debug for Rclone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rclone")
            .field("exe", &self.exe)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// One remote file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFile {
    /// File name.
    #[serde(rename = "Name")]
    pub name: String,
    /// Bytes.
    #[serde(rename = "Size")]
    pub size: u64,
}

impl Rclone {
    /// A runner.
    #[must_use]
    pub fn new(
        exe: impl Into<PathBuf>,
        config: impl Into<PathBuf>,
        pass: impl Into<String>,
    ) -> Self {
        Self {
            exe: exe.into(),
            config: config.into(),
            pass: pass.into(),
        }
    }

    fn run(&self, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>, BackupError> {
        let mut cmd = Command::new(&self.exe);
        cmd.arg("--config")
            .arg(&self.config)
            .arg("--ask-password=false")
            .args(args)
            .env("RCLONE_CONFIG_PASS", &self.pass)
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = cmd
            .output()
            .map_err(|e| BackupError::Rclone(format!("running {}: {e}", self.exe.display())))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no output");
            Err(BackupError::Rclone(format!(
                "rclone {} failed ({}): {last}",
                args.first()
                    .map_or_else(String::new, |a| a.to_string_lossy().into_owned()),
                out.status
            )))
        }
    }

    /// Uploads `local` as `remote_path` (e.g. `gdrive-crypt:name.tar.zst`).
    ///
    /// # Errors
    /// rclone fails.
    pub fn upload(&self, local: &Path, remote_path: &str) -> Result<(), BackupError> {
        self.run(&["copyto".as_ref(), local.as_os_str(), remote_path.as_ref()])
            .map(drop)
    }

    /// Downloads `remote_path` to `local`.
    ///
    /// # Errors
    /// rclone fails.
    pub fn download(&self, remote_path: &str, local: &Path) -> Result<(), BackupError> {
        self.run(&["copyto".as_ref(), remote_path.as_ref(), local.as_os_str()])
            .map(drop)
    }

    /// Files directly under `remote` (e.g. `gdrive-crypt:`).
    ///
    /// # Errors
    /// rclone fails or prints something unexpected.
    pub fn list(&self, remote: &str) -> Result<Vec<RemoteFile>, BackupError> {
        let out = self.run(&["lsjson".as_ref(), "--files-only".as_ref(), remote.as_ref()])?;
        serde_json::from_slice(&out)
            .map_err(|e| BackupError::Rclone(format!("unexpected lsjson output: {e}")))
    }

    /// Deletes one remote file.
    ///
    /// # Errors
    /// rclone fails.
    pub fn delete(&self, remote_path: &str) -> Result<(), BackupError> {
        self.run(&["deletefile".as_ref(), remote_path.as_ref()])
            .map(drop)
    }
}
