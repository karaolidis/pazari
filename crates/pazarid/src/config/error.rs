use thiserror::Error;

use crate::broker::RegistryError;

/// A context ID vsock keeps for itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("context ID {cid} is reserved and no guest can hold it")]
pub struct ReservedCid {
    pub cid: u32,
}

/// A vsock port the daemon cannot listen on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("vsock port {port} cannot be listened on")]
pub struct UnusablePort {
    pub port: u32,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(transparent)]
    File(#[from] pazari_config::ConfigError),

    #[error("the file names no guests")]
    Guestless,

    #[error(transparent)]
    Registry(#[from] RegistryError),
}
