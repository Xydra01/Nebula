//! `nebula backup ...`.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};
use nebula_config::NebulaConfig;
use nebula_resources::doctor::{backup_auth_expiry, record_backup_auth};
use time::OffsetDateTime;

use crate::{Ctx, emit_json};

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
