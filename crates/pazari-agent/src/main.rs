use std::{io, path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{Parser, Subcommand};
use pazari_agent::{
    config::{Config, Settings},
    run,
    runtime::Preflight,
};
use tracing::error;
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_PATH: &str = "/etc/pazari/agent.toml";

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// The agent's configuration file.
    #[arg(long, short, global = true, default_value = DEFAULT_CONFIG_PATH, value_name = "PATH")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Releases the brake on the workload cgroup and exits.
    ///
    /// Run from `ExecStopPost` after any exit.
    PanicOpen,

    /// Checks the guest and prints what the checks found.
    Preflight,

    /// Prints a commented configuration file to stdout.
    ExampleConfig,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    match agent(cli).await {
        Ok(code) => code,
        Err(error) => {
            error!(%error, "the agent failed");
            ExitCode::FAILURE
        }
    }
}

async fn agent(cli: Cli) -> Result<ExitCode> {
    let config = || Config::load(&cli.config);

    match cli.command {
        None => {
            run::run(config()?).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::PanicOpen) => {
            run::panic_open(&config()?.runtime).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Preflight) => {
            let config = config()?;
            let report = Preflight::new(&config.runtime).run(config.engine.reclaim_swappiness);
            println!("{report}");
            Ok(if report.passed() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Some(Command::ExampleConfig) => {
            print!("{}", Config::template());
            Ok(ExitCode::SUCCESS)
        }
    }
}
