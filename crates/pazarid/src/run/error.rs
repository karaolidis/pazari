use std::io;

use pazari_transport::TransportError;
use thiserror::Error;

use crate::runtime::RuntimeError;

#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Transport(#[from] TransportError),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error("failed to install the {signal} handler: {source}")]
    Signal {
        signal: &'static str,
        source: io::Error,
    },
}
