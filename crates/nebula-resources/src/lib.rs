//! Resource monitoring for the daemon and the CLI (PHASE0_PLAN 6.5): GPU, memory and disk
//! snapshots, the disk guard, the commit-charge preflight for model loads, and the doctor
//! checks that don't need the daemon.

pub mod doctor;
mod sampler;
pub mod sources;

use std::sync::Arc;

use nebula_config::VolumeGuard;
use nebula_model::ModelProfile;
use nebula_proto::DiskUsage;

pub use sampler::{ProcessSource, Sampler, SamplerConfig, Sources};
pub use sources::Commit;

/// Resource reading failures.
#[derive(Debug, thiserror::Error)]
pub enum ResourceError {
    /// A source isn't present or refused (no NVIDIA driver, missing counters, ...).
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// Spawning the sampler thread failed.
    #[error("sampler thread: {0}")]
    Thread(#[from] std::io::Error),
}

/// Bytes per "GB" in thresholds: GiB, which is what Explorer shows as GB.
pub const GB: u64 = 1024 * 1024 * 1024;

/// Disk-guard verdict for one volume, from best to worst (PHASE0_PLAN 3.2). Phase 0 only
/// reports these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardLevel {
    /// Plenty of space.
    Ok,
    /// Below the warning threshold.
    Warn,
    /// New work should not start.
    Block,
    /// Running work should pause.
    Pause,
}

impl std::fmt::Display for GuardLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Block => "block",
            Self::Pause => "pause",
        })
    }
}

/// The guard level for `free_bytes` on a volume with these thresholds.
#[must_use]
pub fn disk_guard(free_bytes: u64, guard: &VolumeGuard) -> GuardLevel {
    let below = |gb: Option<u64>| gb.is_some_and(|gb| free_bytes < gb.saturating_mul(GB));
    if below(guard.pause_gb) {
        GuardLevel::Pause
    } else if below(guard.block_gb) {
        GuardLevel::Block
    } else if below(Some(guard.warn_gb)) {
        GuardLevel::Warn
    } else {
        GuardLevel::Ok
    }
}

fn same_mount(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\')
        .eq_ignore_ascii_case(b.trim_end_matches('\\'))
}

/// Guard levels for every guarded volume found in `disks`.
#[must_use]
pub fn guard_disks<'a>(
    disks: &'a [DiskUsage],
    guards: &'a [VolumeGuard],
) -> Vec<(&'a DiskUsage, &'a VolumeGuard, GuardLevel)> {
    guards
        .iter()
        .filter_map(|g| {
            disks
                .iter()
                .find(|d| same_mount(&d.mount, &g.mount))
                .map(|d| (d, g, disk_guard(d.free_bytes, g)))
        })
        .collect()
}

/// Refuses a load whose estimated commit plus `margin_mib` doesn't fit in the headroom.
///
/// # Errors
/// A human-readable reason.
pub fn check_commit(
    profile_name: &str,
    estimate_mib: u64,
    margin_mib: u64,
    commit: &Commit,
) -> Result<(), String> {
    let need = estimate_mib + margin_mib;
    let free = commit.headroom_mib();
    if free >= need {
        Ok(())
    } else {
        Err(format!(
            "profile {profile_name} needs ~{need} MiB of commit charge ({estimate_mib} estimated + \
             {margin_mib} margin) but only {free} MiB is free ({} of {} MiB committed); close \
             memory-heavy programs or raise the page file maximum",
            commit.used_mib,
            commit.effective_limit_mib()
        ))
    }
}

type CommitReader = dyn Fn() -> Result<Commit, ResourceError> + Send + Sync;

/// The [`nebula_model::Preflight`] hook: reads commit charge fresh at launch time (a profile
/// switch has only just released the old server's commit) and applies [`check_commit`].
/// Profiles without `commit_estimate_mib` always pass, and so does a failed reading.
pub struct CommitPreflight {
    margin_mib: u64,
    read: Arc<CommitReader>,
}

impl CommitPreflight {
    /// Reads the real commit charge.
    #[must_use]
    pub fn new(margin_mib: u64) -> Self {
        Self::with_reader(margin_mib, Arc::new(sources::commit_info))
    }

    /// Uses `read` instead of the OS (tests).
    #[must_use]
    pub fn with_reader(margin_mib: u64, read: Arc<CommitReader>) -> Self {
        Self { margin_mib, read }
    }
}

impl nebula_model::Preflight for CommitPreflight {
    fn check(&self, name: &str, profile: &ModelProfile) -> Result<(), String> {
        let Some(estimate) = profile.commit_estimate_mib else {
            return Ok(());
        };
        match (self.read)() {
            Ok(commit) => check_commit(name, estimate, self.margin_mib, &commit),
            Err(e) => {
                tracing::warn!(event = "resources.commit_unreadable", error = %e);
                Ok(())
            }
        }
    }
}
