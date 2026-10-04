//! Human-readable output. Pure functions of their inputs, so they are snapshot-tested.

use std::fmt::Write as _;

use nebula_config::VolumeGuard;
use nebula_proto::{
    CheckStatus, DaemonStatus, DoctorReport, ModelState, ModelStatus, ResourceSnapshot, StopReason,
    Usage,
};
use nebula_resources::{GB, GuardLevel, guard_disks};
use serde::Serialize;
use time::OffsetDateTime;

/// ANSI colors, or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    /// Whether to emit color codes.
    pub color: bool,
}

impl Style {
    /// No color.
    pub const PLAIN: Self = Self { color: false };

    /// Color only on a terminal, and never with `NO_COLOR` set.
    #[must_use]
    pub fn detect() -> Self {
        use std::io::IsTerminal;
        Self {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn paint(self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }

    /// Green.
    #[must_use]
    pub fn good(self, s: &str) -> String {
        self.paint("32", s)
    }
    /// Yellow.
    #[must_use]
    pub fn warn(self, s: &str) -> String {
        self.paint("33", s)
    }
    /// Red.
    #[must_use]
    pub fn bad(self, s: &str) -> String {
        self.paint("31", s)
    }
    /// Dim.
    #[must_use]
    pub fn dim(self, s: &str) -> String {
        self.paint("2", s)
    }
    /// Bold.
    #[must_use]
    pub fn bold(self, s: &str) -> String {
        self.paint("1", s)
    }
}

/// The serde name of an enum value, e.g. `ready`.
#[must_use]
pub fn name_of<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `42s`, `5m 03s`, `2h 03m`, `3d 04h`.
#[must_use]
pub fn duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s if s < 86_400 => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
        s => format!("{}d {:02}h", s / 86_400, s % 86_400 / 3600),
    }
}

fn state(style: Style, s: ModelState) -> String {
    let n = name_of(&s);
    match s {
        ModelState::Ready | ModelState::Busy => style.good(&n),
        ModelState::Failed => style.bad(&n),
        ModelState::Starting
        | ModelState::Restarting
        | ModelState::Stopped
        | ModelState::Unloaded => style.warn(&n),
    }
}

/// `model.status` / `model.set_profile`.
#[must_use]
pub fn model_status(s: &ModelStatus, now: OffsetDateTime, style: Style) -> String {
    let age = u64::try_from((now - s.since).whole_seconds()).unwrap_or(0);
    let mut out = format!(
        "model    {} {} for {}, {} restart(s)\n",
        style.bold(&s.profile),
        state(style, s.state),
        duration(age),
        s.restarts
    );
    if let Some(e) = &s.last_error {
        let _ = writeln!(out, "error    {}", style.bad(e));
    }
    out
}

/// `daemon.status`.
#[must_use]
pub fn daemon_status(s: &DaemonStatus, now: OffsetDateTime, style: Style) -> String {
    let mut out = format!(
        "daemon   {} (pid {}, version {}, protocol {}), up {}\n",
        style.good("running"),
        s.pid,
        s.version,
        s.proto_version,
        duration(s.uptime_s)
    );
    out.push_str(&model_status(&s.model, now, style));
    out
}

fn mib_gb(mib: u64) -> String {
    let tenths = mib * 10 / 1024;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// `resources.snapshot`, with guard levels for the configured volumes.
#[must_use]
pub fn resources(s: &ResourceSnapshot, volumes: &[VolumeGuard], style: Style) -> String {
    let mut out = String::new();
    let util = s
        .gpu_util_pct
        .map_or_else(String::new, |u| format!(", {u}% busy"));
    let _ = writeln!(
        out,
        "VRAM     {} / {} GB{util}",
        mib_gb(s.vram_used_mib),
        mib_gb(s.vram_total_mib)
    );
    let _ = writeln!(
        out,
        "RAM      {} / {} GB",
        mib_gb(s.ram_used_mib),
        mib_gb(s.ram_total_mib)
    );
    let _ = writeln!(
        out,
        "commit   {} / {} GB (limit includes page-file growth)",
        mib_gb(s.commit_used_mib),
        mib_gb(s.commit_limit_mib)
    );
    let _ = writeln!(out, "CPU      {:.0}%", s.cpu_pct);
    for (d, g, level) in guard_disks(&s.disks, volumes) {
        let lvl = name_of(&level);
        let lvl = match level {
            GuardLevel::Ok => style.good(&lvl),
            GuardLevel::Warn => style.warn(&lvl),
            GuardLevel::Block | GuardLevel::Pause => style.bad(&lvl),
        };
        let _ = writeln!(
            out,
            "disk     {:<4} {} / {} GB free  guard {lvl} (warn < {} GB)",
            d.mount,
            d.free_bytes / GB,
            d.total_bytes / GB,
            g.warn_gb
        );
    }
    if !s.gpu_processes.is_empty() {
        let _ = writeln!(out, "\n{}", style.bold("GPU memory by process"));
        for p in &s.gpu_processes {
            let _ = writeln!(out, "  {:>7} MiB  {:>6}  {}", p.vram_mib, p.pid, p.name);
        }
    }
    out
}

fn tag(style: Style, s: CheckStatus) -> String {
    match s {
        CheckStatus::Ok => style.good("ok  "),
        CheckStatus::Warn => style.warn("WARN"),
        CheckStatus::Fail => style.bad("FAIL"),
    }
}

/// `doctor.run`.
#[must_use]
pub fn doctor(r: &DoctorReport, style: Style) -> String {
    let width = r.checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for c in &r.checks {
        let _ = writeln!(
            out,
            "{}  {:<width$}  {}",
            tag(style, c.status),
            c.name,
            c.detail
        );
    }
    let _ = writeln!(out, "\noverall: {}", tag(style, r.overall).trim_end());
    out
}

/// The line printed after each chat reply.
#[must_use]
pub fn usage_line(u: &Usage, stop: StopReason, style: Style) -> String {
    let rate = |n: u32, ms: f64| {
        if ms > 0.0 {
            f64::from(n) * 1000.0 / ms
        } else {
            0.0
        }
    };
    let mut line = format!(
        "[prompt {} tok, {:.1} t/s | reply {} tok, {:.1} t/s",
        u.prompt_n,
        rate(u.prompt_n, u.prompt_ms),
        u.predicted_n,
        rate(u.predicted_n, u.predicted_ms)
    );
    if stop != StopReason::Stop {
        let _ = write!(line, " | stopped: {}", name_of(&stop));
    }
    line.push(']');
    style.dim(&line)
}
