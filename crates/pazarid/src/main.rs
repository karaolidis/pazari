use std::{io, path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{Parser, Subcommand};
use pazarid::{config::Config, run};
use tracing::error;
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_PATH: &str = "/etc/pazari/pazarid.toml";

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// The daemon's configuration file.
    #[arg(long, short, global = true, default_value = DEFAULT_CONFIG_PATH, value_name = "PATH")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
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

    match daemon(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "the daemon failed");
            ExitCode::FAILURE
        }
    }
}

async fn daemon(cli: Cli) -> Result<()> {
    match cli.command {
        None => run::run(Config::load(&cli.config)?).await?,
        Some(Command::ExampleConfig) => print!("{}", Config::template()),
    }

    Ok(())
}
