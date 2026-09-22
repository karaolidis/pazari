use thiserror::Error;

use crate::broker::RegistryError;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(transparent)]
    File(#[from] pazari_config::ConfigError),

    #[error("the file names no guests")]
    Guestless,

    #[error(transparent)]
    Registry(#[from] RegistryError),
}
