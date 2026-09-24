pub mod qmp;

use std::future::Future;

pub use crate::runtime::actuator::qmp::{Mode, QmpActuator};
use crate::runtime::error::RuntimeError;

/// One reading of a memory device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// Total memory size.
    pub size: u64,

    /// What of that size was plugged in, rather than there at boot.
    pub hotplugged: u64,
}

impl Reading {
    #[must_use]
    pub const fn plugged(boot: u64, hotplugged: u64) -> Self {
        Self {
            size: boot.saturating_add(hotplugged),
            hotplugged,
        }
    }

    #[must_use]
    pub const fn sized(size: u64, boot: u64) -> Self {
        Self {
            size,
            hotplugged: size.saturating_sub(boot),
        }
    }
}

/// One guest's memory device.
pub trait Actuator: Send + Sync {
    fn reading(&self) -> impl Future<Output = Result<Reading, RuntimeError>> + Send;

    fn requested(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;

    /// Asks the device for an absolute memory size.
    fn set_requested(&self, bytes: u64) -> impl Future<Output = Result<(), RuntimeError>> + Send;

    /// The memory size, once it differs from the last one reported.
    fn changed(&self) -> impl Future<Output = Result<Reading, RuntimeError>> + Send;
}
