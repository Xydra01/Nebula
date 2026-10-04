//! The `nebula` command line (PHASE0_PLAN 6.7). Everything talks to the daemon over its
//! pipe; `doctor` and `resources` fall back to local checks when the daemon is down.
//! Output is plain text that works over SSH (color only on a terminal, never with
//! `NO_COLOR`), or JSON with `--json`.

mod chat;
mod daemon_ctl;
pub mod format;

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Parser, Subcommand, ValueEnum};
use nebula_config::NebulaConfig;
use nebula_daemon::client::{Client, ClientError};
use nebula_proto::{
    CheckStatus, DaemonStatus, DoctorCheck, DoctorReport, Empty, Event, LogLevel,
    LogsSubscribeParams, Method, ModelSetProfileParams, ModelStatus, ReasoningEffort,
    ResourceSnapshot, TraceId, error_code,
};
use serde::Serialize;
use time::OffsetDateTime;
use tokio::io::AsyncBufRead;

pub use format::Style;

/// How long to wait for a busy pipe.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// `nebula`.
#[derive(Debug, Parser)]
#[command(name = "nebula", version, about = "Nebula, a local-first coding agent")]
pub struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start, stop or inspect the daemon.
    Daemon {
        /// Action.
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Chat with the model, one line per message (/reset clears history, /exit quits).
    Chat(ChatArgs),
    /// Show or switch the model profile.
    Model {
        /// Action.
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Stream the daemon's logs.
    Logs {
        /// Action.
        #[command(subcommand)]
        action: LogsAction,
    },
    /// One resource snapshot, with the top GPU memory users.
    Resources,
    /// Health report; exits 0 (ok), 1 (warnings) or 2 (failures).
    Doctor,
}

/// `nebula daemon ...`.
#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Start the daemon in the background and wait until the model is ready.
    Start,
    /// Stop the daemon gracefully.
    Stop,
    /// Show daemon and model state.
    Status,
}

/// `nebula chat` options.
#[derive(Debug, Clone, clap::Args)]
pub struct ChatArgs {
    /// Model profile (switches the daemon's model if different).
    #[arg(long)]
    pub profile: Option<String>,
    /// JSON Schema file the replies must match.
    #[arg(long)]
    pub schema: Option<PathBuf>,
    /// Reasoning effort.
    #[arg(long, value_enum)]
    pub reasoning: Option<Reasoning>,
    /// Generation limit per reply.
    #[arg(long)]
    pub max_tokens: Option<u32>,
}

/// `--reasoning` values (`high` is not offered; the server rejects it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Reasoning {
    /// Off.
    None,
    /// Short.
    Low,
    /// Medium.
    Medium,
    /// Longest.
    Xhigh,
}

impl From<Reasoning> for ReasoningEffort {
    fn from(r: Reasoning) -> Self {
        match r {
            Reasoning::None => Self::None,
            Reasoning::Low => Self::Low,
            Reasoning::Medium => Self::Medium,
            Reasoning::Xhigh => Self::Xhigh,
        }
    }
}

/// `nebula model ...`.
#[derive(Debug, Subcommand)]
pub enum ModelAction {
    /// Show the model state.
    Status,
    /// Switch to another profile and wait until it is ready.
    Profile {
        /// Profile name from the config.
        name: String,
    },
}

/// `nebula logs ...`.
#[derive(Debug, Subcommand)]
pub enum LogsAction {
    /// Follow new log events until Ctrl-C.
    Tail {
        /// Minimum level.
        #[arg(long, value_enum)]
        level: Option<Level>,
        /// Only this trace.
        #[arg(long)]
        trace: Option<TraceId>,
        /// Only targets starting with this prefix (e.g. `nebula_model`).
        #[arg(long)]
        target: Option<String>,
    },
}

/// `--level` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Level {
    /// Everything.
    Trace,
    /// Diagnostics and up.
    Debug,
    /// Normal operation and up.
    Info,
    /// Warnings and errors.
    Warn,
    /// Errors only.
    Error,
}

impl From<Level> for LogLevel {
    fn from(l: Level) -> Self {
        match l {
            Level::Trace => Self::Trace,
            Level::Debug => Self::Debug,
            Level::Info => Self::Info,
            Level::Warn => Self::Warn,
            Level::Error => Self::Error,
        }
    }
}

/// Everything a command needs besides its arguments.
#[derive(Clone)]
pub struct Ctx {
    /// Loaded config.
    pub config: NebulaConfig,
    /// Daemon pipe path.
    pub pipe: String,
    /// `--json`.
    pub json: bool,
    /// Colors.
    pub style: Style,
    /// Whether stdin is a terminal (prompts and reasoning are shown).
    pub interactive: bool,
    /// `nebula-daemon.exe` to start; next to this executable if `None`.
    pub daemon_exe: Option<PathBuf>,
}

pub(crate) async fn connect(ctx: &Ctx) -> Result<Client, ClientError> {
    Client::connect(&ctx.pipe, CONNECT_TIMEOUT).await
}

pub(crate) fn emit_json<T: Serialize>(out: &mut dyn Write, v: &T) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(&mut *out, v)?;
    writeln!(out)?;
    Ok(())
}

/// Runs one command; returns the process exit code.
///
/// # Errors
/// Connection, protocol or I/O failures (exit code 1 in `main`).
pub async fn run(
    command: Command,
    ctx: &Ctx,
    input: impl AsyncBufRead + Unpin,
    out: &mut dyn Write,
) -> anyhow::Result<u8> {
    match command {
        Command::Daemon { action } => match action {
            DaemonAction::Start => daemon_ctl::start(ctx, out).await,
            DaemonAction::Stop => daemon_ctl::stop(ctx, out).await,
            DaemonAction::Status => daemon_status(ctx, out).await,
        },
        Command::Chat(args) => chat::chat(ctx, &args, input, out).await,
        Command::Model { action } => model(ctx, action, out).await,
        Command::Logs {
            action:
                LogsAction::Tail {
                    level,
                    trace,
                    target,
                },
        } => logs_tail(ctx, level, trace, target, out).await,
        Command::Resources => resources(ctx, out).await,
        Command::Doctor => doctor(ctx, out).await,
    }
}

async fn daemon_status(ctx: &Ctx, out: &mut dyn Write) -> anyhow::Result<u8> {
    let mut c = match connect(ctx).await {
        Err(ClientError::NotRunning(_)) => {
            if ctx.json {
                emit_json(out, &serde_json::json!({ "running": false }))?;
            } else {
                writeln!(out, "daemon   {}", ctx.style.warn("not running"))?;
            }
            return Ok(1);
        }
        r => r?,
    };
    let s: DaemonStatus = c.call(Method::DaemonStatus(Empty {})).await?;
    if ctx.json {
        emit_json(out, &s)?;
    } else {
        write!(
            out,
            "{}",
            format::daemon_status(&s, OffsetDateTime::now_utc(), ctx.style)
        )?;
    }
    Ok(0)
}

async fn model(ctx: &Ctx, action: ModelAction, out: &mut dyn Write) -> anyhow::Result<u8> {
    let mut c = connect(ctx).await?;
    let s: ModelStatus = match action {
        ModelAction::Status => c.call(Method::ModelStatus(Empty {})).await?,
        ModelAction::Profile { name } => {
            if !ctx.json {
                writeln!(out, "switching to {name}...")?;
                out.flush()?;
            }
            c.call(Method::ModelSetProfile(ModelSetProfileParams {
                profile: name,
            }))
            .await?
        }
    };
    if ctx.json {
        emit_json(out, &s)?;
    } else {
        write!(
            out,
            "{}",
            format::model_status(&s, OffsetDateTime::now_utc(), ctx.style)
        )?;
    }
    Ok(0)
}

async fn logs_tail(
    ctx: &Ctx,
    level: Option<Level>,
    trace: Option<TraceId>,
    target: Option<String>,
    out: &mut dyn Write,
) -> anyhow::Result<u8> {
    let mut c = connect(ctx).await?;
    let _: Empty = c
        .call(Method::LogsSubscribe(LogsSubscribeParams {
            min_level: level.map(LogLevel::from),
            trace_id: trace,
            target_prefix: target,
        }))
        .await?;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        let ev = tokio::select! {
            _ = &mut ctrl_c => return Ok(0),
            ev = c.next_event() => ev,
        };
        match ev {
            Ok(Event::LogEvent(e)) => {
                if ctx.json {
                    serde_json::to_writer(&mut *out, &e)?;
                    writeln!(out)?;
                } else {
                    writeln!(out, "{}", nebula_telemetry::layer::console_line(&e))?;
                }
                out.flush()?;
            }
            Ok(_) => {}
            Err(ClientError::Closed) => {
                writeln!(out, "(the daemon closed the connection)")?;
                return Ok(1);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn local_snapshot(config: &NebulaConfig) -> anyhow::Result<ResourceSnapshot> {
    let mounts: Vec<String> = config
        .resources
        .volumes
        .iter()
        .map(|v| v.mount.clone())
        .collect();
    let (snap, _) = nebula_resources::Sources::real().snapshot(&mounts)?;
    Ok(snap)
}

async fn resources(ctx: &Ctx, out: &mut dyn Write) -> anyhow::Result<u8> {
    let snap = match connect(ctx).await {
        Ok(mut c) => {
            let mut tries = 0;
            loop {
                match c
                    .call::<ResourceSnapshot>(Method::ResourcesSnapshot(Empty {}))
                    .await
                {
                    Ok(s) => break s,
                    Err(e) if e.rpc_code() == Some(error_code::INTERNAL_ERROR) && tries < 20 => {
                        tries += 1;
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Err(ClientError::NotRunning(_)) => {
            let config = ctx.config.clone();
            tokio::task::spawn_blocking(move || local_snapshot(&config)).await??
        }
        Err(e) => return Err(e.into()),
    };
    if ctx.json {
        emit_json(out, &snap)?;
    } else {
        write!(
            out,
            "{}",
            format::resources(&snap, &ctx.config.resources.volumes, ctx.style)
        )?;
    }
    Ok(0)
}

/// The exit code for a doctor verdict.
#[must_use]
pub const fn doctor_exit_code(s: CheckStatus) -> u8 {
    match s {
        CheckStatus::Ok => 0,
        CheckStatus::Warn => 1,
        CheckStatus::Fail => 2,
    }
}

async fn doctor(ctx: &Ctx, out: &mut dyn Write) -> anyhow::Result<u8> {
    let report = match connect(ctx).await {
        Ok(mut c) => {
            if !ctx.json && ctx.interactive {
                writeln!(
                    out,
                    "{}",
                    ctx.style
                        .dim("running checks (the first run hashes the model files)...")
                )?;
                out.flush()?;
            }
            let s: DaemonStatus = c.call(Method::DaemonStatus(Empty {})).await?;
            let r: DoctorReport = c.call(Method::DoctorRun(Empty {})).await?;
            let mut checks = vec![DoctorCheck {
                name: "daemon".into(),
                status: CheckStatus::Ok,
                detail: format!(
                    "running (pid {}, up {})",
                    s.pid,
                    format::duration(s.uptime_s)
                ),
            }];
            checks.extend(r.checks);
            DoctorReport::from_checks(checks)
        }
        Err(ClientError::NotRunning(_)) => {
            let config = ctx.config.clone();
            let local = tokio::task::spawn_blocking(move || {
                nebula_resources::doctor::local_checks(&config)
            })
            .await
            .context("local checks")?;
            let mut checks = vec![DoctorCheck {
                name: "daemon".into(),
                status: CheckStatus::Fail,
                detail:
                    "not running (start it with `nebula daemon start`); showing local checks only"
                        .into(),
            }];
            checks.extend(local.checks);
            DoctorReport::from_checks(checks)
        }
        Err(e) => return Err(e.into()),
    };
    if ctx.json {
        emit_json(out, &report)?;
    } else {
        write!(out, "{}", format::doctor(&report, ctx.style))?;
    }
    Ok(doctor_exit_code(report.overall))
}
