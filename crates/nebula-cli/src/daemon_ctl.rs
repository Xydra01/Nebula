//! `nebula daemon start|stop`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use nebula_daemon::client::ClientError;
use nebula_proto::{DaemonStatus, Empty, Method, ModelState};
use time::OffsetDateTime;

use crate::{Ctx, connect, emit_json, format};

/// Longest wait for the startup model to load (page-file growth can make it slow).
const START_TIMEOUT: Duration = Duration::from_secs(300);
/// Longest wait for a graceful stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(90);
/// The daemon's exit code when another instance is running.
const EXIT_ALREADY_RUNNING: i32 = 3;

fn daemon_exe(ctx: &Ctx) -> anyhow::Result<PathBuf> {
    if let Some(p) = &ctx.daemon_exe {
        return Ok(p.clone());
    }
    let me = std::env::current_exe().context("locating nebula.exe")?;
    let exe = me.with_file_name("nebula-daemon.exe");
    if !exe.exists() {
        bail!(
            "{} not found (it is installed next to nebula.exe)",
            exe.display()
        );
    }
    Ok(exe)
}

/// Starts the daemon detached from this console, and outside this console's job if
/// allowed, so it survives the terminal or SSH session closing.
fn spawn_detached(exe: &PathBuf) -> anyhow::Result<Child> {
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const ERROR_ACCESS_DENIED: i32 = 5;
        crate::win::stop_inheriting_std_handles();
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        cmd.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                cmd.creation_flags(base);
            }
            r => return r.with_context(|| format!("starting {}", exe.display())),
        }
    }
    cmd.spawn()
        .with_context(|| format!("starting {}", exe.display()))
}

fn print_status(ctx: &Ctx, s: &DaemonStatus, out: &mut dyn Write) -> anyhow::Result<()> {
    if ctx.json {
        emit_json(out, s)
    } else {
        write!(
            out,
            "{}",
            format::daemon_status(s, OffsetDateTime::now_utc(), ctx.style)
        )?;
        Ok(())
    }
}

pub(crate) async fn start(ctx: &Ctx, out: &mut dyn Write) -> anyhow::Result<u8> {
    if let Ok(mut c) = connect(ctx).await {
        let s: DaemonStatus = c.call(Method::DaemonStatus(Empty {})).await?;
        if !ctx.json {
            writeln!(out, "already running")?;
        }
        print_status(ctx, &s, out)?;
        return Ok(0);
    }
    let exe = daemon_exe(ctx)?;
    let mut child = spawn_detached(&exe)?;
    let want = ctx.config.daemon.load_on_start.clone();
    if !ctx.json {
        writeln!(out, "started {} (pid {})", exe.display(), child.id())?;
        out.flush()?;
    }
    let deadline = Instant::now() + START_TIMEOUT;
    let mut last: Option<(String, ModelState)> = None;
    loop {
        if let Some(code) = child.try_wait()? {
            if code.code() == Some(EXIT_ALREADY_RUNNING) {
                bail!(
                    "another daemon is already running (its pipe isn't answering yet; try again)"
                );
            }
            bail!(
                "nebula-daemon exited with {code}; see the log in {}",
                ctx.config.paths.logs.display()
            );
        }
        if Instant::now() > deadline {
            bail!(
                "the daemon did not become ready within {}s",
                START_TIMEOUT.as_secs()
            );
        }
        if let Ok(mut c) = connect(ctx).await {
            let s: DaemonStatus = c.call(Method::DaemonStatus(Empty {})).await?;
            let m = &s.model;
            let now = (m.profile.clone(), m.state);
            if !ctx.json && last.as_ref() != Some(&now) {
                writeln!(out, "model {} is {}", m.profile, format::name_of(&m.state))?;
                out.flush()?;
                last = Some(now);
            }
            let loaded =
                m.profile == want && matches!(m.state, ModelState::Ready | ModelState::Busy);
            if want.is_empty() || loaded {
                print_status(ctx, &s, out)?;
                return Ok(0);
            }
            if m.state == ModelState::Failed
                || (m.state == ModelState::Stopped && m.last_error.is_some())
            {
                print_status(ctx, &s, out)?;
                return Ok(1);
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub(crate) async fn stop(ctx: &Ctx, out: &mut dyn Write) -> anyhow::Result<u8> {
    let mut c = match connect(ctx).await {
        Err(ClientError::NotRunning(_)) => {
            if ctx.json {
                emit_json(out, &serde_json::json!({ "running": false }))?;
            } else {
                writeln!(out, "not running")?;
            }
            return Ok(0);
        }
        r => r?,
    };
    let status: DaemonStatus = c.call(Method::DaemonStatus(Empty {})).await?;
    let _: Empty = c.call(Method::DaemonShutdown(Empty {})).await?;
    drop(c);
    if !ctx.json {
        writeln!(out, "stopping...")?;
        out.flush()?;
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    let still_running = || {
        anyhow::anyhow!(
            "the daemon is still running after {}s",
            STOP_TIMEOUT.as_secs()
        )
    };
    loop {
        match connect(ctx).await {
            Err(ClientError::NotRunning(_)) => break,
            _ if Instant::now() > deadline => return Err(still_running()),
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
    // The pipe closes before the model servers are stopped; wait for the process itself.
    // An in-process daemon (tests) shares our pid and is stopped by its owner.
    if status.pid != std::process::id() {
        let pid = sysinfo::Pid::from_u32(status.pid);
        let mut sys = sysinfo::System::new();
        loop {
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
            if sys.process(pid).is_none() {
                break;
            }
            if Instant::now() > deadline {
                return Err(still_running());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    if ctx.json {
        emit_json(out, &serde_json::json!({ "running": false }))?;
    } else {
        writeln!(out, "stopped")?;
    }
    Ok(0)
}
