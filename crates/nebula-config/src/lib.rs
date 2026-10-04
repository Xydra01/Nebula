//! Typed Nebula configuration.
//!
//! The shipped defaults (`config/default.toml`) are embedded in the binary. A local override
//! file, if present, is merged over them table by table: tables merge recursively, and any
//! other value (including arrays) replaces the default. Unknown keys are an error.

use std::path::{Path, PathBuf};

use nebula_model::ModelConfig;
use nebula_telemetry::TelemetryConfig;
use serde::Deserialize;

/// The shipped defaults.
pub const DEFAULT_TOML: &str = include_str!("../../../config/default.toml");

/// Environment variable naming an override file, instead of [`DEFAULT_OVERRIDE_PATH`].
pub const OVERRIDE_ENV: &str = "NEBULA_CONFIG";

/// Where the local override lives unless [`OVERRIDE_ENV`] says otherwise.
pub const DEFAULT_OVERRIDE_PATH: &str = r"F:\Nebula\config\nebula.toml";

/// Config loading failures.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A file could not be read.
    #[error("reading {path}: {source}")]
    Io {
        /// File.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// TOML syntax or a type/unknown-key error.
    #[error("{origin}: {message}")]
    Parse {
        /// Which document failed.
        origin: String,
        /// Parser message.
        message: String,
    },
    /// The config parsed but is inconsistent or unsafe.
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// Where Nebula keeps its data.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paths {
    /// Root of hot data, e.g. `F:\Nebula`.
    pub data_root: PathBuf,
    /// Daily JSONL logs and `blobs\`.
    pub logs: PathBuf,
    /// Daemon state (doctor history, hash cache, SMART JSON, ...).
    pub state: PathBuf,
    /// Log archive on the cold drive.
    pub archive: PathBuf,
    /// Local copies of backups (PHASE0_PLAN 7.1).
    pub backups_local: PathBuf,
}

/// The `[daemon]` section.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// Pipe name; the full path is `\\.\pipe\<name>`.
    pub pipe_name: String,
    /// Chat profile loaded at startup; empty for none.
    #[serde(default)]
    pub load_on_start: String,
    /// Embedding profile run next to the chat model; empty to disable.
    #[serde(default)]
    pub embedding_profile: String,
    /// Credential Manager targets whose values are registered with the redactor.
    #[serde(default)]
    pub secrets: Vec<String>,
}

impl DaemonConfig {
    /// Full pipe path.
    #[must_use]
    pub fn pipe_path(&self) -> String {
        format!(r"\\.\pipe\{}", self.pipe_name)
    }
}

/// Disk-guard thresholds for one volume (PHASE0_PLAN 3.2).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeGuard {
    /// Mount point, e.g. `F:\`.
    pub mount: String,
    /// Warn below this many GB free.
    pub warn_gb: u64,
    /// Block new work (or archive writes) below this.
    #[serde(default)]
    pub block_gb: Option<u64>,
    /// Pause running work below this.
    #[serde(default)]
    pub pause_gb: Option<u64>,
}

/// The `[resources]` section.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcesConfig {
    /// Snapshot interval.
    pub sample_interval_ms: u64,
    /// One snapshot is logged every this many seconds.
    pub log_interval_s: u64,
    /// Commit charge kept free on top of a profile's estimate.
    pub commit_margin_mib: u64,
    /// Doctor warns when free VRAM is below this.
    pub vram_headroom_warn_mib: u64,
    /// Hot log budget on the data drive.
    pub log_budget_gb: u64,
    /// Log archive cap on the cold drive.
    pub archive_cap_gb: u64,
    /// Drive letter that must not be used, e.g. `C:`.
    pub retired_drive: String,
    /// Disk-guard thresholds.
    #[serde(default)]
    pub volumes: Vec<VolumeGuard>,
}

/// The `[backup]` section (PHASE0_PLAN 7.1).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupConfig {
    /// The rclone executable.
    pub rclone: PathBuf,
    /// rclone's config file, encrypted with the `nebula/rclone_config_pass` credential.
    pub rclone_config: PathBuf,
    /// The cloud remote that holds the OAuth sign-in, e.g. `gdrive:`.
    pub auth_remote: String,
    /// The encrypted remote backups are written to, e.g. `gdrive-crypt:`.
    pub remote: String,
    /// How long a sign-in lasts (7 for a Google app in Testing); 0 if it doesn't expire.
    pub token_lifetime_days: u64,
    /// Doctor warns this many days before the sign-in expires.
    pub token_warn_days: u64,
}

/// The whole configuration.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NebulaConfig {
    /// Data locations.
    pub paths: Paths,
    /// Off-site backups.
    pub backup: BackupConfig,
    /// Logging; `log_dir` defaults to `paths.logs`.
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// Daemon settings.
    pub daemon: DaemonConfig,
    /// Resource sampling and thresholds.
    pub resources: ResourcesConfig,
    /// Model runtimes and profiles.
    pub model: ModelConfig,
}

fn parse_table(text: &str, origin: &str) -> Result<toml::Table, ConfigError> {
    text.parse::<toml::Table>().map_err(|e| ConfigError::Parse {
        origin: origin.to_owned(),
        message: e.to_string(),
    })
}

/// Merges `over` into `base`: tables recursively, everything else replaced.
pub fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

impl NebulaConfig {
    /// Parses the defaults merged with an optional override document, then validates.
    ///
    /// # Errors
    /// [`ConfigError::Parse`] or [`ConfigError::Invalid`].
    pub fn from_toml(override_toml: Option<&str>) -> Result<Self, ConfigError> {
        let mut table = parse_table(DEFAULT_TOML, "config/default.toml")?;
        if let Some(text) = override_toml {
            merge(&mut table, parse_table(text, "override")?);
        }
        let mut cfg: Self = table
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError::Parse {
                origin: "merged config".into(),
                message: e.to_string(),
            })?;
        if cfg.telemetry.log_dir.is_none() {
            cfg.telemetry.log_dir = Some(cfg.paths.logs.clone());
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// The override file path: `$NEBULA_CONFIG` or [`DEFAULT_OVERRIDE_PATH`].
    #[must_use]
    pub fn override_path() -> PathBuf {
        std::env::var_os(OVERRIDE_ENV).map_or_else(|| DEFAULT_OVERRIDE_PATH.into(), PathBuf::from)
    }

    /// Loads defaults plus the override at `path`, if that file exists.
    ///
    /// # Errors
    /// I/O errors other than a missing file, parse errors, or validation errors.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(Some(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::from_toml(None),
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Loads defaults plus the override at [`NebulaConfig::override_path`].
    ///
    /// # Errors
    /// See [`NebulaConfig::load_from`].
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&Self::override_path())
    }

    /// Every configured path, labelled, for drive checks.
    #[must_use]
    pub fn all_paths(&self) -> Vec<(String, PathBuf)> {
        let mut out = vec![
            ("paths.data_root".to_owned(), self.paths.data_root.clone()),
            ("paths.logs".to_owned(), self.paths.logs.clone()),
            ("paths.state".to_owned(), self.paths.state.clone()),
            ("paths.archive".to_owned(), self.paths.archive.clone()),
            (
                "paths.backups_local".to_owned(),
                self.paths.backups_local.clone(),
            ),
            (
                "backup.rclone_config".to_owned(),
                self.backup.rclone_config.clone(),
            ),
        ];
        if let Some(d) = &self.telemetry.log_dir {
            out.push(("telemetry.log_dir".to_owned(), d.clone()));
        }
        for (name, p) in &self.model.runtimes {
            out.push((format!("model.runtimes.{name}"), p.clone()));
        }
        for (name, p) in &self.model.profiles {
            out.push((format!("model.profiles.{name}.model"), p.model.clone()));
            if let Some(b) = &p.kv_bias {
                out.push((format!("model.profiles.{name}.kv_bias"), b.clone()));
            }
        }
        out
    }

    /// Configured paths on the retired drive.
    #[must_use]
    pub fn paths_on_retired_drive(&self) -> Vec<(String, PathBuf)> {
        let drive = self.resources.retired_drive.to_ascii_uppercase();
        self.all_paths()
            .into_iter()
            .filter(|(_, p)| {
                p.to_string_lossy()
                    .get(..drive.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&drive))
            })
            .collect()
    }

    /// Checks profiles and runtimes, the default/embedding profile names, and that nothing
    /// points at the retired drive.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] describing the first problem.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.model
            .validate()
            .map_err(|e| ConfigError::Invalid(e.to_string()))?;
        for (what, name) in [
            ("daemon.load_on_start", &self.daemon.load_on_start),
            ("daemon.embedding_profile", &self.daemon.embedding_profile),
        ] {
            if !name.is_empty() && !self.model.profiles.contains_key(name) {
                return Err(ConfigError::Invalid(format!(
                    "{what} names unknown profile {name:?}"
                )));
            }
        }
        if let Some((name, p)) = self.paths_on_retired_drive().into_iter().next() {
            return Err(ConfigError::Invalid(format!(
                "{name} = {} is on the retired drive {}",
                p.display(),
                self.resources.retired_drive
            )));
        }
        if self.daemon.pipe_name.is_empty() || self.daemon.pipe_name.contains('\\') {
            return Err(ConfigError::Invalid(
                "daemon.pipe_name must be a bare name".into(),
            ));
        }
        Ok(())
    }
}
