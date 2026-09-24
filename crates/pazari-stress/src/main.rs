use std::{
    io::{self, StdoutLock},
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use anyhow::Result;
use bytesize::ByteSize;
use clap::Parser;
use pazari_stress::{Fill, Options, Plan, Reporter, run};
use pazari_systemd::logging;
use pazari_types::Interval;
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    flag,
};
use tracing::error;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// How often a running phase reports its progress.
    #[arg(long, default_value = "500ms", value_name = "DURATION", value_parser = humantime::parse_duration)]
    interval: Duration,

    /// How much address space to map. Defaults to the plan's own peak.
    #[arg(long, value_name = "SIZE")]
    arena: Option<ByteSize>,

    /// Map the arena `MAP_NORESERVE`, so it is never committed up front.
    #[arg(long)]
    no_reserve: bool,

    /// Keep transparent huge pages out of the arena.
    #[arg(long)]
    no_thp: bool,

    /// What every touched page is filled with.
    #[arg(long, default_value = "index", value_name = "index|random")]
    fill: Fill,

    /// The phases to run, in order:
    /// `grow=SIZE[,rate=SIZE/s][,chunk=SIZE]`,
    /// `churn=TIME,rate=SIZE/s[,chunk=SIZE]`,
    /// `hold=TIME`, or
    /// `release=SIZE[,rate=SIZE/s][,chunk=SIZE]`.
    #[arg(required = true, value_name = "PHASE")]
    phases: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    logging::install("info");

    let mut out = Reporter::new(io::stdout().lock());

    match stress(&cli, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "workload failed");

            if let Err(error) = out.failed(&error.to_string()) {
                error!(%error, "failed to report the error");
            }

            ExitCode::FAILURE
        }
    }
}

fn stress(cli: &Cli, out: &mut Reporter<StdoutLock<'static>>) -> Result<()> {
    let plan = Plan::parse(&cli.phases)?;

    let options = Options {
        arena: plan.arena(cli.arena.map(|arena| arena.as_u64()))?,
        interval: Interval::try_new(cli.interval)?,
        no_reserve: cli.no_reserve,
        no_thp: cli.no_thp,
        fill: cli.fill,
        plan,
    };

    let interrupt = Arc::new(AtomicBool::new(false));

    for signal in [SIGTERM, SIGINT] {
        flag::register(signal, Arc::clone(&interrupt))?;
    }

    run::run(&options, out, &interrupt)?;

    Ok(())
}
