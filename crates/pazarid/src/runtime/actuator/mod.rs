pub mod qmp;

use std::future::Future;

pub use crate::runtime::actuator::qmp::{Mode, QmpActuator};
use crate::runtime::error::RuntimeError;

/// One guest's memory device.
pub trait Actuator: Send + Sync {
    fn size(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;

    fn requested(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;

    /// Asks the device for an absolute memory size.
    fn set_requested(&self, bytes: u64) -> impl Future<Output = Result<(), RuntimeError>> + Send;

    /// The memory size, once it differs from the last one reported.
    fn changed(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;
}
