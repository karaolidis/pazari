use std::io;

use thiserror::Error;

use crate::{
    engine::EngineError,
    runtime::{Report, RuntimeError},
};

#[derive(Debug, Error)]
pub enum RunError {
    #[error("the guest is not fit to run the agent:\n{report}")]
    Preflight { report: Report },

    #[error(transparent)]
    Engine(#[from] EngineError),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error("failed to install the {signal} handler: {source}")]
    Signal {
        signal: &'static str,
        source: io::Error,
    },

    #[error("the transport session is gone")]
    SessionLost,
}
