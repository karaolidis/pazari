mod exponential;

use std::num::NonZeroU64;

use enum_dispatch::enum_dispatch;
use pazari_types::Timestamp;

use crate::engine::config::GrowConfig;
pub use crate::engine::grow::exponential::Exponential;

/// How much more memory each request asks the host for.
#[enum_dispatch]
pub trait Grow {
    /// How far over the current total the next request reaches.
    fn step(&self) -> NonZeroU64;

    /// The workload hit the brake, and a request is on its way.
    fn braked(&mut self, now: Timestamp);

    /// A grow has landed.
    fn settled(&mut self, now: Timestamp);

    /// Forget every earlier grow.
    fn reset(&mut self);
}

#[enum_dispatch(Grow)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrowStrategy {
    Exponential(Exponential),
}

impl From<GrowConfig> for GrowStrategy {
    fn from(config: GrowConfig) -> Self {
        match config {
            GrowConfig::Exponential(config) => Self::Exponential(Exponential::new(config)),
        }
    }
}
