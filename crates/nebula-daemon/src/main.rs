//! `nebula-daemon`: wires the real dependencies into [`nebula_daemon::run`].

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use nebula_config::NebulaConfig;
use nebula_daemon::{DaemonError, Deps, INSTANCE_MUTEX};
use nebula_model::{ProcessLauncher, SupervisorConfig};
use nebula_resources::{CommitPreflight, Sources};
use nebula_tools::ToolHost;

/// Exit code when another daemon is already running.
const EXIT_ALREADY_RUNNING: u8 = 3;
/// Days of logs kept on the hot drive.
const HOT_LOG_DAYS: u16 = 7;
/// How often logs are archived.
const MAINTENANCE_EVERY: Duration = Duration::from_secs(6 * 3600);

#[tokio::main]
#[allow(clippy::print_stderr)]
async fn main() -> ExitCode {
    match real_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if matches!(e.downcast_ref(), Some(DaemonError::AlreadyRunning)) => {
            eprintln!("nebula-daemon: {e}");
            ExitCode::from(EXIT_ALREADY_RUNNING)
        }
        Err(e) => {
            eprintln!("nebula-daemon: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn real_main() -> anyhow::Result<()> {
    let config = NebulaConfig::load().context("loading config")?;
    let telemetry = nebula_telemetry::init(&config.telemetry).context("starting telemetry")?;
    let found = nebula_daemon::register_secrets(&telemetry, &config.daemon.secrets);
    tracing::info!(
        event = "daemon.secrets_registered",
        found,
        configured = config.daemon.secrets.len()
    );
    spawn_log_maintenance(&config);
    let tool_host = ToolHost::start(&config.tools, telemetry.blobs().cloned())
        .await
        .context("starting the tool host")?;
    let deps = Deps {
        telemetry,
        launcher: Arc::new(ProcessLauncher::default()),
        tool_host: Arc::new(tool_host),
        sources: Some(Sources::real()),
        preflight: Some(Arc::new(CommitPreflight::new(
            config.resources.commit_margin_mib,
        ))),
        supervisor: SupervisorConfig::default(),
        instance_name: INSTANCE_MUTEX.to_owned(),
        local_checks: Arc::new(nebula_resources::doctor::local_checks),
        artifact_checks: true,
    };
    nebula_daemon::run(config, deps).await?;
    Ok(())
}

/// Moves old daily logs to the archive and keeps the archive under its cap.
fn spawn_log_maintenance(config: &NebulaConfig) {
    let Some(logs) = config.telemetry.log_dir.clone() else {
        return;
    };
    let archive = config.paths.archive.clone();
    let cap = config.resources.archive_cap_gb.saturating_mul(1 << 30);
    tokio::spawn(async move {
        loop {
            let (logs, archive) = (logs.clone(), archive.clone());
            let _ = tokio::task::spawn_blocking(move || {
                let today = time::OffsetDateTime::now_utc().date();
                match nebula_telemetry::budget::archive_old_logs(
                    &logs,
                    &archive,
                    today,
                    HOT_LOG_DAYS,
                ) {
                    Ok(r) if !r.moved.is_empty() => {
                        tracing::info!(
                            event = "logs.archived",
                            files = r.moved.len(),
                            bytes = r.bytes
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(event = "logs.archive_failed", error = %e),
                }
                if let Err(e) = nebula_telemetry::budget::enforce_archive_cap(&archive, cap) {
                    tracing::warn!(event = "logs.archive_prune_failed", error = %e);
                }
            })
            .await;
            tokio::time::sleep(MAINTENANCE_EVERY).await;
        }
    });
}
