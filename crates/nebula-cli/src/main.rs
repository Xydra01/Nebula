//! `nebula`.

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use nebula_cli::{Cli, Ctx, Style};
use nebula_config::NebulaConfig;
use nebula_daemon::client::ClientError;

#[tokio::main]
#[allow(clippy::print_stderr)]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match NebulaConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ctx = Ctx {
        pipe: config.daemon.pipe_path(),
        config,
        json: cli.json,
        style: if cli.json {
            Style::PLAIN
        } else {
            Style::detect()
        },
        interactive: std::io::stdin().is_terminal(),
        daemon_exe: None,
    };
    let input = tokio::io::BufReader::new(tokio::io::stdin());
    let mut out = std::io::stdout();
    match nebula_cli::run(cli.command, &ctx, input, &mut out).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            if matches!(e.downcast_ref(), Some(ClientError::NotRunning(_))) {
                eprintln!(
                    "error: the Nebula daemon is not running; start it with `nebula daemon start`"
                );
            } else {
                eprintln!("error: {e:#}");
            }
            ExitCode::FAILURE
        }
    }
}
