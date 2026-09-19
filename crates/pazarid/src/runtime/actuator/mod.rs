use std::future::Future;

use crate::runtime::error::RuntimeError;

/// One guest's memory device.
pub trait Actuator: Send + Sync {
    fn size(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;

    fn requested(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;

    /// Asks the device for an absolute size.
    fn set_requested(&self, bytes: u64) -> impl Future<Output = Result<(), RuntimeError>> + Send;

    /// Resolves with the actual size the next time it moves.
    fn changed(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;
}
