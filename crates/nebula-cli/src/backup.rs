//! `nebula backup ...`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};
use nebula_backup::{Options, Outcome, Rclone, RemoteFile};
use nebula_config::NebulaConfig;
use nebula_resources::doctor::{backup_auth_expiry, record_backup_auth};
use serde::Serialize;
use time::OffsetDateTime;

use crate::{Ctx, emit_json};

fn runner(cfg: &NebulaConfig) -> anyhow::Result<Rclone> {
    Ok(Rclone::new(
        &cfg.backup.rclone,
        &cfg.backup.rclone_config,
        config_pass()?,
    ))
}

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
}

pub(crate) fn now(
    ctx: &Ctx,
    if_changed: bool,
    allow_large_delete: bool,
    out: &mut dyn Write,
) -> anyhow::Result<u8> {
    let cfg = &ctx.config;
    let rc = runner(cfg)?;
    let opts = Options {
        if_changed,
        allow_large_delete,
    };
    let outcome = nebula_backup::backup_now(cfg, &rc, opts, OffsetDateTime::now_utc())?;
    match outcome {
        Outcome::Unchanged => {
            if ctx.json {
                emit_json(out, &serde_json::json!({ "skipped": "unchanged" }))?;
            } else {
                writeln!(out, "nothing changed since the last backup; skipped")?;
            }
        }
        Outcome::Done(r) => {
            if ctx.json {
                emit_json(
                    out,
                    &serde_json::json!({
                        "name": r.name,
                        "files": r.files,
                        "bytes": r.bytes,
                        "pruned_remote": r.pruned_remote,
                        "pruned_local": r.pruned_local,
                    }),
                )?;
            } else {
                writeln!(
                    out,
                    "{} ({} files, {}) saved to {} and uploaded to {}",
                    r.name,
                    r.files,
                    mib(r.bytes),
                    cfg.paths.backups_local.display(),
                    cfg.backup.remote
                )?;
                if !r.pruned_remote.is_empty() || !r.pruned_local.is_empty() {
                    writeln!(
                        out,
                        "retention removed {} cloud and {} local backup(s)",
                        r.pruned_remote.len(),
                        r.pruned_local.len()
                    )?;
                }
            }
        }
    }
    Ok(0)
}

#[derive(Serialize)]
struct Listing {
    local: Vec<RemoteFile>,
    remote: Vec<RemoteFile>,
    remote_error: Option<String>,
}

pub(crate) fn list(ctx: &Ctx, out: &mut dyn Write) -> anyhow::Result<u8> {
    let cfg = &ctx.config;
    let local = nebula_backup::local_backups(cfg)?;
    let (remote, remote_error) =
        match runner(cfg).and_then(|rc| Ok(nebula_backup::remote_backups(cfg, &rc)?)) {
            Ok(r) => (r, None),
            Err(e) => (Vec::new(), Some(format!("{e:#}"))),
        };
    if ctx.json {
        emit_json(
            out,
            &Listing {
                local,
                remote,
                remote_error,
            },
        )?;
        return Ok(0);
    }
    let mut names: Vec<&str> = local
        .iter()
        .chain(&remote)
        .map(|f| f.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    writeln!(out, "{:<34} {:>11}  where", "backup", "size")?;
    for n in names {
        let l = local.iter().find(|f| f.name == n);
        let r = remote.iter().find(|f| f.name == n);
        let size = l.or(r).map_or(0, |f| f.size);
        let place = match (l.is_some(), r.is_some()) {
            (true, true) => "local + cloud",
            (true, false) => "local",
            _ => "cloud",
        };
        let id = n.strip_suffix(".tar.zst").unwrap_or(n);
        writeln!(out, "{id:<34} {:>11}  {place}", mib(size))?;
    }
    if let Some(e) = remote_error {
        writeln!(
            out,
            "{}",
            ctx.style.warn(&format!("cloud listing failed: {e}"))
        )?;
        return Ok(1);
    }
    Ok(0)
}

pub(crate) fn restore(
    ctx: &Ctx,
    id: &str,
    to: Option<PathBuf>,
    in_place: bool,
    out: &mut dyn Write,
) -> anyhow::Result<u8> {
    let cfg = &ctx.config;
    let name = nebula_backup::normalize_id(id);
    let dest = to.unwrap_or_else(|| nebula_backup::default_restore_dir(cfg, &name));
    let rc = runner(cfg).ok();
    let manifest = nebula_backup::restore(cfg, rc.as_ref(), id, &dest)?;
    writeln!(
        out,
        "{} files from {name} restored and verified in {}",
        manifest.files.len(),
        dest.display()
    )?;
    if in_place {
        let written = nebula_backup::apply_in_place(&manifest, &dest)?;
        writeln!(
            out,
            "{} files copied over the live state and config",
            written.len()
        )?;
    }
    Ok(0)
}

/// Credential Manager target holding the rclone config password.
pub const RCLONE_PASS_TARGET: &str = "nebula/rclone_config_pass";

fn rclone(cfg: &NebulaConfig, pass: &str) -> Command {
    let mut cmd = Command::new(&cfg.backup.rclone);
    cmd.arg("--config")
        .arg(&cfg.backup.rclone_config)
        .arg("--ask-password=false")
        .env("RCLONE_CONFIG_PASS", pass);
    cmd
}

fn config_pass() -> anyhow::Result<String> {
    nebula_daemon::win::read_credential(RCLONE_PASS_TARGET).with_context(|| {
        format!("no {RCLONE_PASS_TARGET} in Credential Manager; run scripts/store-rclone-pass.ps1")
    })
}

/// Renews the off-site sign-in (interactive: rclone opens a browser), checks it works, and
/// records the time for `doctor`. With `record_only`, skips the renewal (right after
/// `rclone config` created the remote).
pub(crate) fn reauth(ctx: &Ctx, record_only: bool, out: &mut dyn Write) -> anyhow::Result<u8> {
    let cfg = &ctx.config;
    if !cfg.backup.rclone_config.exists() {
        bail!(
            "{} not found; create the remotes first (docs/ops/backup.md)",
            cfg.backup.rclone_config.display()
        );
    }
    let pass = config_pass()?;
    let remote = &cfg.backup.auth_remote;
    if !record_only {
        writeln!(
            out,
            "Renewing {remote}: answer y to refresh the token, then sign in in the browser."
        )?;
        out.flush()?;
        let status = rclone(cfg, &pass)
            .args(["config", "reconnect", remote])
            .status()
            .with_context(|| format!("running {}", cfg.backup.rclone.display()))?;
        if !status.success() {
            bail!("rclone config reconnect failed ({status})");
        }
    }
    let probe = rclone(cfg, &pass)
        .args(["lsf", "--max-depth", "1", remote])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {}", cfg.backup.rclone.display()))?;
    if !probe.status.success() {
        let err = String::from_utf8_lossy(&probe.stderr);
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        bail!("{remote} doesn't accept the sign-in: {last}");
    }
    let now = OffsetDateTime::now_utc();
    record_backup_auth(cfg, now).context("recording the sign-in")?;
    let expires = backup_auth_expiry(cfg, now);
    if ctx.json {
        emit_json(
            out,
            &serde_json::json!({
                "remote": remote,
                "signed_in_at": now.unix_timestamp(),
                "expires_at": expires.map(OffsetDateTime::unix_timestamp),
            }),
        )?;
    } else {
        match expires {
            Some(e) => writeln!(
                out,
                "{remote} works. The sign-in expires {}; doctor warns {} day(s) before.",
                e.date(),
                cfg.backup.token_warn_days
            )?,
            None => writeln!(out, "{remote} works.")?,
        }
    }
    Ok(0)
}
