use std::{path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{Parser, Subcommand};
use pazari_systemd::logging;
use pazarid::{
    config::{Config, Settings},
    run,
};
use tracing::error;

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

    logging::install("info");

    match daemon(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "daemon failed");
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
