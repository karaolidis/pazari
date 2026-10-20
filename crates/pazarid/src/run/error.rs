use std::{fmt, io};

use pazari_metrics::MetricsError;
use pazari_transport::TransportError;
use thiserror::Error;

use crate::runtime::RuntimeError;

/// A signal the daemon stops on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Terminate,
    Interrupt,
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Terminate => "SIGTERM",
            Self::Interrupt => "SIGINT",
        })
    }
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Transport(#[from] TransportError),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error(transparent)]
    Metrics(#[from] MetricsError),

    #[error("failed to install the {signal} handler: {source}")]
    Signal { signal: Signal, source: io::Error },

    #[error(
        "the daemon was asked to take memory back under pressure, and this host carries no {path} to measure it with"
    )]
    Pressureless { path: &'static str },
}
