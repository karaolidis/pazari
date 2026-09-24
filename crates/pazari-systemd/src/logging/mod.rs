use std::io::{self, IsTerminal};

use tracing::debug;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

/// Installs the process-wide log subscriber, filtered by `RUST_LOG`.
pub fn install(default: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let directives = filter.to_string();

    let refused = match systemd_journal_logger::connected_to_journal().then(tracing_journald::layer)
    {
        Some(Ok(journal)) => {
            tracing_subscriber::registry()
                .with(filter)
                .with(journal)
                .init();

            debug!(directives, "logging natively to the journal");

            return;
        }
        Some(Err(error)) => Some(error),
        None => None,
    };

    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .with_env_filter(filter)
        .init();

    if let Some(error) = refused {
        debug!(
            %error,
            "journal is capturing stderr but rejected native logging; falling back to stderr",
        );
    }

    debug!(directives, "logging to stderr");
}
