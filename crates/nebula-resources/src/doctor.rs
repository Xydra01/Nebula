//! Doctor checks that don't need the daemon, so `nebula doctor` can run them when the
//! daemon is down. Inputs are gathered by [`gather`] (OS calls, one PowerShell run, file
//! reads) and judged by the pure [`evaluate`], which is what the tests drive.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nebula_config::NebulaConfig;
use nebula_proto::{CheckStatus, DiskUsage, DoctorCheck, DoctorReport};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::sources::{Commit, GpuReading, GpuSource, NvmlGpu, SysinfoSystem, SystemSource};
use crate::{GB, GuardLevel, guard_disks};

/// GPT partition type of an EFI system partition.
pub const EFI_GPT_TYPE: &str = "{c12a7328-f81f-11d2-ba4b-00a0c93ec93b}";

/// Remote address ranges an SSH rule may allow: Tailscale's IPv4 CGNAT and IPv6 ranges, in
/// the CIDR and mask forms Windows reports.
pub const TAILSCALE_RANGES: &[&str] = &[
    "100.64.0.0/10",
    "100.64.0.0/255.192.0.0",
    "fd7a:115c:a1e0::/48",
];

/// SMART snapshots older than this are stale.
pub const SMART_MAX_AGE: Duration = Duration::from_secs(35 * 24 * 3600);

/// An off-site backup older than this is stale (the nightly run always uploads).
pub const BACKUP_MAX_AGE: Duration = Duration::from_secs(36 * 3600);

/// How far back the first disk-event check looks.
pub const FIRST_EVENT_LOOKBACK: Duration = Duration::from_secs(7 * 24 * 3600);

/// One partition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Partition {
    /// Disk number.
    pub disk: u32,
    /// Drive letter, if any.
    #[serde(default)]
    pub letter: Option<String>,
    /// GPT type GUID, lowercase with braces (empty for MBR).
    #[serde(default)]
    pub gpt_type: String,
}

/// One System-log event about a disk or NTFS.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DiskEvent {
    /// When, RFC 3339.
    pub time: String,
    /// Provider name.
    pub provider: String,
    /// Event id.
    pub id: u32,
    /// First line of the message.
    #[serde(default)]
    pub message: String,
}

/// An inbound allow rule for TCP 22.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct FirewallRule {
    /// Rule name.
    pub name: String,
    /// Whether it is enabled.
    pub enabled: bool,
    /// Allowed remote addresses (`*` for all).
    #[serde(default)]
    pub remote: Vec<String>,
}

/// Facts read through PowerShell.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct SystemFacts {
    /// Page file paths.
    #[serde(default)]
    pub page_files: Vec<String>,
    /// All partitions.
    #[serde(default)]
    pub partitions: Vec<Partition>,
    /// Disk and NTFS error events since the last check.
    #[serde(default)]
    pub disk_events: Vec<DiskEvent>,
    /// Inbound TCP 22 allow rules.
    #[serde(default)]
    pub ssh_rules: Vec<FirewallRule>,
    /// `wsl --status` succeeded; `None` if `wsl.exe` is missing.
    #[serde(default)]
    pub wsl: Option<bool>,
}

/// Off-site backups, from `state\backup-last.json` (written by `nebula backup now`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupState {
    /// Age of the last successful upload; `None` if there never was one.
    pub uploaded_age: Option<Duration>,
    /// Its archive name.
    pub uploaded_name: Option<String>,
    /// The latest run's error.
    pub last_error: Option<String>,
}

#[derive(Deserialize)]
struct BackupRecord {
    #[serde(default, with = "time::serde::rfc3339::option")]
    last_uploaded_at: Option<OffsetDateTime>,
    #[serde(default)]
    last_uploaded: Option<String>,
    #[serde(default)]
    last_error: Option<String>,
}

/// The off-site remote's sign-in, as recorded by `nebula backup reauth`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupAuth {
    /// No rclone config file yet.
    NoRcloneConfig,
    /// The config exists but no sign-in was recorded.
    Unrecorded,
    /// Last sign-in.
    SignedIn(OffsetDateTime),
}

/// `state\backup-auth.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupAuthRecord {
    /// The remote that was signed in to.
    pub remote: String,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub signed_in_at: OffsetDateTime,
}

fn backup_auth_file(cfg: &NebulaConfig) -> PathBuf {
    cfg.paths.state.join("backup-auth.json")
}

/// Records a sign-in to the off-site remote.
///
/// # Errors
/// The state file can't be written.
pub fn record_backup_auth(cfg: &NebulaConfig, at: OffsetDateTime) -> std::io::Result<()> {
    let path = backup_auth_file(cfg);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let record = BackupAuthRecord {
        remote: cfg.backup.auth_remote.clone(),
        signed_in_at: at,
    };
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&record).map_err(std::io::Error::other)?,
    )
}

/// Reads the recorded sign-in. A record for a different remote counts as none.
#[must_use]
pub fn backup_auth(cfg: &NebulaConfig) -> BackupAuth {
    if !cfg.backup.rclone_config.exists() {
        return BackupAuth::NoRcloneConfig;
    }
    std::fs::read_to_string(backup_auth_file(cfg))
        .ok()
        .and_then(|t| serde_json::from_str::<BackupAuthRecord>(&t).ok())
        .filter(|r| r.remote == cfg.backup.auth_remote)
        .map_or(BackupAuth::Unrecorded, |r| {
            BackupAuth::SignedIn(r.signed_in_at)
        })
}

/// When the recorded sign-in expires; `None` if it doesn't.
#[must_use]
pub fn backup_auth_expiry(cfg: &NebulaConfig, signed_in: OffsetDateTime) -> Option<OffsetDateTime> {
    let days = i64::try_from(cfg.backup.token_lifetime_days).unwrap_or(i64::MAX);
    (days > 0).then(|| signed_in.saturating_add(time::Duration::days(days)))
}

/// Everything [`evaluate`] judges.
#[derive(Clone, Debug)]
pub struct LocalInputs {
    /// NVML reading.
    pub gpu: Result<GpuReading, String>,
    /// Guarded volumes that exist.
    pub disks: Vec<DiskUsage>,
    /// Commit charge.
    pub commit: Result<Commit, String>,
    /// PowerShell facts.
    pub facts: Result<SystemFacts, String>,
    /// Start of the disk-event window.
    pub events_since: OffsetDateTime,
    /// `(file name, contents)` of SMART JSON files; `None` if the folder is missing.
    pub smart_files: Option<Vec<(String, String)>>,
    /// Local backups.
    pub backups: BackupState,
    /// Off-site sign-in.
    pub backup_auth: BackupAuth,
    /// Hot log bytes (logs + blobs).
    pub log_bytes: Result<u64, String>,
    /// Whether a file could be created in the log directory.
    pub log_writable: Result<(), String>,
    /// Now.
    pub now: OffsetDateTime,
}

fn check(name: impl Into<String>, status: CheckStatus, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        status,
        detail: detail.into(),
    }
}

/// Judges the inputs.
#[must_use]
pub fn evaluate(cfg: &NebulaConfig, i: &LocalInputs) -> Vec<DoctorCheck> {
    let mut out = vec![gpu_check(cfg, &i.gpu)];
    out.extend(disk_checks(cfg, &i.disks));
    out.push(commit_check(cfg, &i.commit));
    out.extend(retired_drive_checks(cfg, &i.facts));
    out.push(disk_events_check(&i.facts, i.events_since));
    out.extend(smart_checks(i.smart_files.as_deref(), i.now));
    out.push(backup_check(&i.backups));
    out.push(backup_auth_check(cfg, &i.backup_auth, i.now));
    out.push(wsl_check(&i.facts));
    out.push(ssh_check(&i.facts));
    out.extend(log_checks(cfg, &i.log_bytes, &i.log_writable));
    out
}

fn mib_gb(mib: u64) -> String {
    let tenths = mib * 10 / 1024;
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn gpu_check(cfg: &NebulaConfig, gpu: &Result<GpuReading, String>) -> DoctorCheck {
    match gpu {
        Err(e) => check("gpu", CheckStatus::Fail, format!("no GPU visible: {e}")),
        Ok(g) if g.vram_total_mib == 0 => check("gpu", CheckStatus::Fail, "GPU reports no VRAM"),
        Ok(g) => {
            let free = g.vram_total_mib.saturating_sub(g.vram_used_mib);
            let detail = format!(
                "{} (driver {}): {} of {} GB VRAM used, {} GB free",
                g.name,
                g.driver,
                mib_gb(g.vram_used_mib),
                mib_gb(g.vram_total_mib),
                mib_gb(free)
            );
            let status = if free < cfg.resources.vram_headroom_warn_mib {
                CheckStatus::Warn
            } else {
                CheckStatus::Ok
            };
            check("gpu", status, detail)
        }
    }
}

fn disk_checks(cfg: &NebulaConfig, disks: &[DiskUsage]) -> Vec<DoctorCheck> {
    let guarded = guard_disks(disks, &cfg.resources.volumes);
    cfg.resources
        .volumes
        .iter()
        .map(|g| {
            let name = format!("disk.{}", g.mount.trim_end_matches('\\'));
            let Some((d, _, level)) = guarded.iter().find(|(_, vg, _)| vg.mount == g.mount) else {
                return check(
                    name,
                    CheckStatus::Fail,
                    format!("{} is not mounted", g.mount),
                );
            };
            let status = match level {
                GuardLevel::Ok => CheckStatus::Ok,
                // Only the hot drive (the one that can pause work) is critical when full.
                GuardLevel::Block | GuardLevel::Pause if g.pause_gb.is_some() => CheckStatus::Fail,
                GuardLevel::Warn | GuardLevel::Block | GuardLevel::Pause => CheckStatus::Warn,
            };
            check(
                name,
                status,
                format!(
                    "{} of {} GB free (guard: {level}; warn below {} GB)",
                    d.free_bytes / GB,
                    d.total_bytes / GB,
                    g.warn_gb
                ),
            )
        })
        .collect()
}

fn commit_check(cfg: &NebulaConfig, commit: &Result<Commit, String>) -> DoctorCheck {
    match commit {
        Err(e) => check(
            "memory.commit",
            CheckStatus::Warn,
            format!("unreadable: {e}"),
        ),
        Ok(c) => {
            let free = c.headroom_mib();
            let status = if free < cfg.resources.commit_margin_mib {
                CheckStatus::Warn
            } else {
                CheckStatus::Ok
            };
            check(
                "memory.commit",
                status,
                format!(
                    "{} of {} GB committed (limit includes page-file growth), {} GB free",
                    mib_gb(c.used_mib),
                    mib_gb(c.effective_limit_mib()),
                    mib_gb(free)
                ),
            )
        }
    }
}

fn letter_of(drive: &str) -> String {
    drive.trim_end_matches(['\\', ':']).to_ascii_uppercase()
}

fn retired_drive_checks(
    cfg: &NebulaConfig,
    facts: &Result<SystemFacts, String>,
) -> Vec<DoctorCheck> {
    let drive = &cfg.resources.retired_drive;
    let letter = letter_of(drive);
    let mut out = Vec::new();
    let on_retired = cfg.paths_on_retired_drive();
    out.push(if on_retired.is_empty() {
        check(
            "paths.retired_drive",
            CheckStatus::Ok,
            format!("no Nebula path on {drive}"),
        )
    } else {
        let list: Vec<String> = on_retired
            .iter()
            .map(|(n, p)| format!("{n} = {}", p.display()))
            .collect();
        check("paths.retired_drive", CheckStatus::Fail, list.join("; "))
    });
    let f = match facts {
        Ok(f) => f,
        Err(e) => {
            out.push(check(
                "retired_drive",
                CheckStatus::Warn,
                format!("system facts unavailable: {e}"),
            ));
            return out;
        }
    };
    let retired_disks: Vec<u32> = f
        .partitions
        .iter()
        .filter(|p| {
            p.letter
                .as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case(&letter))
        })
        .map(|p| p.disk)
        .collect();
    out.push(if retired_disks.is_empty() {
        check(
            "retired_drive.attached",
            CheckStatus::Ok,
            format!("{drive} is not attached"),
        )
    } else {
        check(
            "retired_drive.attached",
            CheckStatus::Warn,
            format!("{drive} is attached (disk {retired_disks:?}) but retired"),
        )
    });
    let efi: Vec<&Partition> = f
        .partitions
        .iter()
        .filter(|p| p.gpt_type.eq_ignore_ascii_case(EFI_GPT_TYPE))
        .collect();
    out.push(if efi.is_empty() {
        check(
            "boot.efi",
            CheckStatus::Warn,
            "no EFI system partition found",
        )
    } else if efi.iter().any(|p| retired_disks.contains(&p.disk)) {
        check(
            "boot.efi",
            CheckStatus::Fail,
            format!("an EFI partition is on the retired drive's disk {retired_disks:?}"),
        )
    } else {
        let disks: Vec<u32> = efi.iter().map(|p| p.disk).collect();
        check(
            "boot.efi",
            CheckStatus::Ok,
            format!("EFI partition on disk {disks:?}"),
        )
    });
    let on_c: Vec<&String> = f
        .page_files
        .iter()
        .filter(|p| letter_of(p.get(..2).unwrap_or_default()) == letter)
        .collect();
    out.push(if f.page_files.is_empty() {
        check("memory.pagefile", CheckStatus::Warn, "no page file")
    } else if on_c.is_empty() {
        check("memory.pagefile", CheckStatus::Ok, f.page_files.join(", "))
    } else {
        check(
            "memory.pagefile",
            CheckStatus::Fail,
            format!(
                "page file on the retired drive: {}",
                f.page_files.join(", ")
            ),
        )
    });
    out
}

fn disk_events_check(facts: &Result<SystemFacts, String>, since: OffsetDateTime) -> DoctorCheck {
    let since = since.format(&Rfc3339).unwrap_or_default();
    match facts {
        Err(e) => check(
            "disk.events",
            CheckStatus::Warn,
            format!("system facts unavailable: {e}"),
        ),
        Ok(f) if f.disk_events.is_empty() => check(
            "disk.events",
            CheckStatus::Ok,
            format!("no disk or NTFS errors since {since}"),
        ),
        Ok(f) => {
            let first = &f.disk_events[0];
            check(
                "disk.events",
                CheckStatus::Warn,
                format!(
                    "{} disk/NTFS error event(s) since {since}; latest: {} {} id {}: {}",
                    f.disk_events.len(),
                    first.time,
                    first.provider,
                    first.id,
                    first.message
                ),
            )
        }
    }
}

/// ATA attributes whose raw value should never grow: reallocated sectors, reported
/// uncorrectable, pending sectors, offline uncorrectable.
const ATA_WATCH: &[(u64, &str)] = &[
    (5, "reallocated"),
    (187, "reported uncorrectable"),
    (197, "pending"),
    (198, "offline uncorrectable"),
];

#[derive(Debug, Default)]
struct Smart {
    serial: String,
    model: String,
    passed: Option<bool>,
    temp: Option<i64>,
    media_errors: Option<u64>,
    spare: Option<(u64, u64)>,
    used_pct: Option<u64>,
    ata: BTreeMap<u64, u64>,
}

fn parse_smart(text: &str) -> Option<Smart> {
    let v: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let nvme = &v["nvme_smart_health_information_log"];
    let mut ata = BTreeMap::new();
    if let Some(table) = v["ata_smart_attributes"]["table"].as_array() {
        for a in table {
            if let (Some(id), Some(raw)) = (a["id"].as_u64(), a["raw"]["value"].as_u64()) {
                ata.insert(id, raw);
            }
        }
    }
    Some(Smart {
        serial: v["serial_number"].as_str()?.to_owned(),
        model: v["model_name"].as_str().unwrap_or_default().to_owned(),
        passed: v["smart_status"]["passed"].as_bool(),
        temp: v["temperature"]["current"].as_i64(),
        media_errors: nvme["media_errors"].as_u64(),
        spare: nvme["available_spare"]
            .as_u64()
            .zip(nvme["available_spare_threshold"].as_u64()),
        used_pct: nvme["percentage_used"].as_u64(),
        ata,
    })
}

/// `dev_sdc-2026-10-01_1932.json` -> `("sdc", "2026-10-01_1932")`.
fn split_smart_name(name: &str) -> Option<(&str, &str)> {
    let stem = name.strip_suffix(".json")?.strip_prefix("dev_")?;
    stem.split_once('-')
}

fn batch_time(stamp: &str) -> Option<OffsetDateTime> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]_[hour][minute]");
    time::PrimitiveDateTime::parse(stamp, &fmt)
        .ok()
        .map(time::PrimitiveDateTime::assume_utc)
}

/// SMART checks from snapshot files (see [`LocalInputs::smart_files`]). Only devices in
/// the newest batch are judged; each is compared with its previous snapshot by serial.
#[must_use]
pub fn smart_checks(files: Option<&[(String, String)]>, now: OffsetDateTime) -> Vec<DoctorCheck> {
    let not_set_up = || {
        vec![check(
            "smart",
            CheckStatus::Warn,
            "not set up (no SMART snapshots in state\\smart; PHASE0_PLAN 1.13)",
        )]
    };
    let Some(files) = files else {
        return not_set_up();
    };
    let mut parsed: Vec<(&str, &str, Smart)> = files
        .iter()
        .filter_map(|(name, text)| {
            let (dev, stamp) = split_smart_name(name)?;
            Some((dev, stamp, parse_smart(text)?))
        })
        .collect();
    parsed.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
    let Some(newest) = parsed.last().map(|p| p.1) else {
        return not_set_up();
    };
    let mut out = Vec::new();
    let age = batch_time(newest).map(|t| now - t);
    out.push(match age {
        Some(a) if a > SMART_MAX_AGE => check(
            "smart.age",
            CheckStatus::Warn,
            format!("newest snapshot {newest} is {} days old", a.whole_days()),
        ),
        Some(a) => check(
            "smart.age",
            CheckStatus::Ok,
            format!("newest snapshot {newest} ({} days old)", a.whole_days()),
        ),
        None => check(
            "smart.age",
            CheckStatus::Warn,
            format!("can't read the time in {newest}"),
        ),
    });
    for (dev, stamp, cur) in parsed.iter().filter(|p| p.1 == newest) {
        let prev = parsed
            .iter()
            .rev()
            .find(|p| p.1 < *stamp && p.2.serial == cur.serial)
            .map(|p| &p.2);
        out.push(judge_smart(dev, cur, prev));
    }
    out
}

fn judge_smart(dev: &str, cur: &Smart, prev: Option<&Smart>) -> DoctorCheck {
    let mut status = CheckStatus::Ok;
    let mut notes: Vec<String> = Vec::new();
    let mut facts: Vec<String> = vec![cur.model.clone()];
    if let Some(t) = cur.temp {
        facts.push(format!("{t} C"));
    }
    if cur.passed == Some(false) {
        status = CheckStatus::Fail;
        notes.push("SMART overall status FAILED".into());
    }
    if let Some(m) = cur.media_errors {
        facts.push(format!("media errors {m}"));
        if let Some(p) = prev.and_then(|p| p.media_errors).filter(|p| m > *p) {
            status = status.max(CheckStatus::Warn);
            notes.push(format!("media errors rose {p} -> {m}"));
        }
    }
    if let Some((spare, threshold)) = cur.spare {
        facts.push(format!("spare {spare}% (threshold {threshold}%)"));
        if spare < threshold {
            status = CheckStatus::Fail;
            notes.push("available spare below threshold".into());
        }
    }
    if let Some(u) = cur.used_pct {
        facts.push(format!("{u}% used"));
    }
    for (id, label) in ATA_WATCH {
        let Some(raw) = cur.ata.get(id) else { continue };
        facts.push(format!("{label} {raw}"));
        if let Some(p) = prev.and_then(|p| p.ata.get(id)).filter(|p| raw > *p) {
            status = status.max(CheckStatus::Warn);
            notes.push(format!("{label} rose {p} -> {raw}"));
        }
    }
    let mut detail = facts.join(", ");
    if !notes.is_empty() {
        detail = format!("{}; {detail}", notes.join("; "));
    }
    check(format!("smart.{dev}"), status, detail)
}

fn backup_check(b: &BackupState) -> DoctorCheck {
    let failed = b
        .last_error
        .as_deref()
        .map_or_else(String::new, |e| format!("; last run failed: {e}"));
    let Some(age) = b.uploaded_age else {
        return check(
            "backup",
            CheckStatus::Fail,
            format!("no successful off-site backup yet (`nebula backup now`){failed}"),
        );
    };
    let hours = age.as_secs() / 3600;
    let name = b.uploaded_name.as_deref().unwrap_or("?");
    if age > BACKUP_MAX_AGE {
        check(
            "backup",
            CheckStatus::Warn,
            format!("last off-site backup {hours} h ago ({name}){failed}"),
        )
    } else if b.last_error.is_some() {
        check(
            "backup",
            CheckStatus::Warn,
            format!("last off-site backup {hours} h ago{failed}"),
        )
    } else {
        check(
            "backup",
            CheckStatus::Ok,
            format!("last off-site backup {hours} h ago ({name})"),
        )
    }
}

fn backup_auth_check(cfg: &NebulaConfig, auth: &BackupAuth, now: OffsetDateTime) -> DoctorCheck {
    const NAME: &str = "backup.auth";
    const FIX: &str = "run `nebula backup reauth`";
    let remote = &cfg.backup.auth_remote;
    match auth {
        BackupAuth::NoRcloneConfig => check(
            NAME,
            CheckStatus::Warn,
            format!(
                "no rclone config at {} (docs/ops/backup.md)",
                cfg.backup.rclone_config.display()
            ),
        ),
        BackupAuth::Unrecorded => check(
            NAME,
            CheckStatus::Warn,
            format!("sign-in date for {remote} unknown; {FIX}"),
        ),
        BackupAuth::SignedIn(at) => {
            let Some(expiry) = backup_auth_expiry(cfg, *at) else {
                return check(
                    NAME,
                    CheckStatus::Ok,
                    format!("{remote} sign-in does not expire"),
                );
            };
            let left = expiry - now;
            let when = expiry.date();
            let warn_days = i64::try_from(cfg.backup.token_warn_days).unwrap_or(i64::MAX);
            if left <= time::Duration::ZERO {
                check(
                    NAME,
                    CheckStatus::Fail,
                    format!("{remote} sign-in expired {when}; off-site backups stopped; {FIX}"),
                )
            } else if left <= time::Duration::days(warn_days) {
                check(
                    NAME,
                    CheckStatus::Warn,
                    format!(
                        "{remote} sign-in expires in {} h ({when}); {FIX}",
                        left.whole_hours()
                    ),
                )
            } else {
                check(
                    NAME,
                    CheckStatus::Ok,
                    format!(
                        "{remote} sign-in expires in {} days ({when})",
                        left.whole_days()
                    ),
                )
            }
        }
    }
}

fn wsl_check(facts: &Result<SystemFacts, String>) -> DoctorCheck {
    match facts.as_ref().map(|f| f.wsl) {
        Err(e) => check(
            "wsl",
            CheckStatus::Warn,
            format!("system facts unavailable: {e}"),
        ),
        Ok(Some(true)) => check("wsl", CheckStatus::Ok, "WSL2 present"),
        Ok(Some(false)) => check("wsl", CheckStatus::Warn, "`wsl --status` failed"),
        Ok(None) => check("wsl", CheckStatus::Warn, "wsl.exe not found"),
    }
}

fn ssh_check(facts: &Result<SystemFacts, String>) -> DoctorCheck {
    let f = match facts {
        Ok(f) => f,
        Err(e) => {
            return check(
                "ssh.firewall",
                CheckStatus::Warn,
                format!("system facts unavailable: {e}"),
            );
        }
    };
    let enabled: Vec<&FirewallRule> = f.ssh_rules.iter().filter(|r| r.enabled).collect();
    if enabled.is_empty() {
        return check(
            "ssh.firewall",
            CheckStatus::Warn,
            "no enabled inbound allow rule for TCP 22",
        );
    }
    let open: Vec<String> = enabled
        .iter()
        .filter(|r| {
            r.remote.is_empty()
                || r.remote
                    .iter()
                    .any(|a| !TAILSCALE_RANGES.iter().any(|t| t.eq_ignore_ascii_case(a)))
        })
        .map(|r| format!("{} allows {}", r.name, r.remote.join(", ")))
        .collect();
    if open.is_empty() {
        let names: Vec<&str> = enabled.iter().map(|r| r.name.as_str()).collect();
        check(
            "ssh.firewall",
            CheckStatus::Ok,
            format!("TCP 22 limited to Tailscale ({})", names.join(", ")),
        )
    } else {
        check("ssh.firewall", CheckStatus::Fail, open.join("; "))
    }
}

fn log_checks(
    cfg: &NebulaConfig,
    bytes: &Result<u64, String>,
    writable: &Result<(), String>,
) -> Vec<DoctorCheck> {
    let budget = cfg.resources.log_budget_gb * GB;
    let usage = match bytes {
        Err(e) => check("logs.budget", CheckStatus::Warn, format!("unreadable: {e}")),
        Ok(b) => check(
            "logs.budget",
            if *b > budget {
                CheckStatus::Warn
            } else {
                CheckStatus::Ok
            },
            format!(
                "{} MB of {} GB",
                b / (1024 * 1024),
                cfg.resources.log_budget_gb
            ),
        ),
    };
    let dir = cfg
        .telemetry
        .log_dir
        .as_deref()
        .unwrap_or(&cfg.paths.logs)
        .display()
        .to_string();
    let write = match writable {
        Ok(()) => check("logs.writable", CheckStatus::Ok, dir),
        Err(e) => check("logs.writable", CheckStatus::Fail, format!("{dir}: {e}")),
    };
    vec![usage, write]
}

/// `state\doctor.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DoctorState {
    /// End of the last disk-event window, RFC 3339.
    #[serde(default)]
    pub last_disk_check: Option<String>,
}

fn state_file(cfg: &NebulaConfig) -> PathBuf {
    cfg.paths.state.join("doctor.json")
}

fn load_state(cfg: &NebulaConfig) -> DoctorState {
    std::fs::read_to_string(state_file(cfg))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_state(cfg: &NebulaConfig, state: &DoctorState) {
    let path = state_file(cfg);
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?,
        )
    };
    if let Err(e) = write() {
        tracing::warn!(event = "doctor.state_unwritable", path = %path.display(), error = %e);
    }
}

fn read_smart_files(dir: &Path) -> Option<Vec<(String, String)>> {
    let entries = std::fs::read_dir(dir).ok()?;
    Some(
        entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let ext_json = Path::new(&name)
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("json"));
                if !ext_json {
                    return None;
                }
                std::fs::read_to_string(e.path()).ok().map(|t| (name, t))
            })
            .collect(),
    )
}

fn backup_state(cfg: &NebulaConfig, now: OffsetDateTime) -> BackupState {
    let Some(r) = std::fs::read_to_string(cfg.paths.state.join("backup-last.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<BackupRecord>(&t).ok())
    else {
        return BackupState::default();
    };
    BackupState {
        uploaded_age: r
            .last_uploaded_at
            .map(|t| (now - t).try_into().unwrap_or_default()),
        uploaded_name: r.last_uploaded,
        last_error: r.last_error,
    }
}

fn probe_writable(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let probe = dir.join(format!(".doctor-probe-{}", std::process::id()));
    std::fs::write(&probe, b"ok").map_err(|e| e.to_string())?;
    std::fs::remove_file(&probe).map_err(|e| e.to_string())
}

/// The PowerShell that produces [`SystemFacts`] as JSON. `{since}` is replaced by the
/// event window start (RFC 3339).
const FACTS_SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$since = [datetime]::Parse('{since}').ToUniversalTime().ToLocalTime()
$pf = @(Get-CimInstance Win32_PageFileUsage | ForEach-Object { [string]$_.Name })
$parts = @(Get-Partition | ForEach-Object { [pscustomobject]@{ disk = [int]$_.DiskNumber; letter = $(if ($_.DriveLetter -and $_.DriveLetter -ne [char]0) { [string]$_.DriveLetter } else { $null }); gpt_type = ([string]$_.GptType).ToLower() } })
$ev = @(Get-WinEvent -FilterHashtable @{ LogName = 'System'; StartTime = $since } -MaxEvents 5000 | Where-Object { ($_.ProviderName -eq 'disk' -and @(7, 51, 153) -contains $_.Id) -or ($_.ProviderName -like '*Ntfs*' -and $_.Level -ge 1 -and $_.Level -le 2) } | Select-Object -First 50 | ForEach-Object { [pscustomobject]@{ time = $_.TimeCreated.ToUniversalTime().ToString('o'); provider = [string]$_.ProviderName; id = [int]$_.Id; message = ([string]$_.Message -split "`n")[0].Trim() } })
$ssh = @((New-Object -ComObject HNetCfg.FwPolicy2).Rules | Where-Object { $_.Direction -eq 1 -and $_.Action -eq 1 -and $_.Protocol -eq 6 -and (([string]$_.LocalPorts) -split ',') -contains '22' } | ForEach-Object { [pscustomobject]@{ name = [string]$_.Name; enabled = [bool]$_.Enabled; remote = @(([string]$_.RemoteAddresses) -split ',' | Where-Object { $_ }) } })
$wsl = $null
if (Get-Command wsl.exe) { wsl.exe --status *> $null; $wsl = ($LASTEXITCODE -eq 0) }
[pscustomobject]@{ page_files = $pf; partitions = $parts; disk_events = $ev; ssh_rules = $ssh; wsl = $wsl } | ConvertTo-Json -Depth 5 -Compress
"#;

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= c.len() {
                out.push(char::from(T[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Runs a PowerShell script (no window, no profile) and returns stdout, killing it after
/// `timeout`.
///
/// # Errors
/// Spawn failure, timeout, or a non-zero exit.
pub fn run_powershell(script: &str, timeout: Duration) -> Result<String, String> {
    use std::process::{Command, Stdio};
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
    ])
    .arg(base64(&utf16))
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| format!("powershell: {e}"))?;
    let mut stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = std::io::Read::read_to_end(s, &mut buf);
        }
        let _ = tx.send(buf);
    });
    let Ok(buf) = rx.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("powershell timed out after {}s", timeout.as_secs()));
    };
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("powershell exited with {status}"));
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Reads [`SystemFacts`], with disk events after `since`.
///
/// # Errors
/// PowerShell failures or unparseable output.
pub fn system_facts(since: OffsetDateTime) -> Result<SystemFacts, String> {
    let since = since.format(&Rfc3339).map_err(|e| e.to_string())?;
    let out = run_powershell(
        &FACTS_SCRIPT.replace("{since}", &since),
        Duration::from_secs(90),
    )?;
    serde_json::from_str(out.trim()).map_err(|e| format!("unexpected output: {e}"))
}

/// Gathers the inputs (blocking: NVML, PowerShell, disk reads) and advances the disk-event
/// window in `state\doctor.json`.
#[must_use]
pub fn gather(cfg: &NebulaConfig) -> LocalInputs {
    let now = OffsetDateTime::now_utc();
    let mut state = load_state(cfg);
    let events_since = state
        .last_disk_check
        .as_deref()
        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
        .unwrap_or(now - FIRST_EVENT_LOOKBACK);
    let gpu = NvmlGpu::new()
        .and_then(|mut g| g.read())
        .map_err(|e| e.to_string());
    let mounts: Vec<String> = cfg
        .resources
        .volumes
        .iter()
        .map(|v| v.mount.clone())
        .collect();
    let disks = SysinfoSystem::new()
        .read(&mounts)
        .map(|r| r.disks)
        .unwrap_or_default();
    let facts = system_facts(events_since);
    if facts.is_ok() {
        state.last_disk_check = now.format(&Rfc3339).ok();
        save_state(cfg, &state);
    }
    let log_dir = cfg
        .telemetry
        .log_dir
        .clone()
        .unwrap_or_else(|| cfg.paths.logs.clone());
    LocalInputs {
        gpu,
        disks,
        commit: crate::sources::commit_info().map_err(|e| e.to_string()),
        facts,
        events_since,
        smart_files: read_smart_files(&cfg.paths.state.join("smart")),
        backups: backup_state(cfg, now),
        backup_auth: backup_auth(cfg),
        log_bytes: nebula_telemetry::budget::usage(&log_dir)
            .map(|u| u.log_bytes + u.blob_bytes)
            .map_err(|e| e.to_string()),
        log_writable: probe_writable(&log_dir),
        now,
    }
}

/// [`gather`] then [`evaluate`].
#[must_use]
pub fn local_checks(cfg: &NebulaConfig) -> DoctorReport {
    DoctorReport::from_checks(evaluate(cfg, &gather(cfg)))
}
