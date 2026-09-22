use std::{io, net::SocketAddr};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("failed to bind the metrics endpoint to {address}: {source}")]
    Bind {
        address: SocketAddr,
        source: io::Error,
    },
}
