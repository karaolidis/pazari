mod titrate;

use std::num::NonZeroU64;

use enum_dispatch::enum_dispatch;
use pazari_types::Timestamp;

pub use crate::engine::release::titrate::Titrate;
use crate::engine::{config::ReleaseConfig, counters::Pressure};

/// When the guest gives memory back to the host, and how much.
#[enum_dispatch]
pub trait Release {
    /// One interval's worth of what the guest has to spare.
    fn observe(&mut self, slack: Option<u64>, pressure: Pressure);

    /// Whether the guest has had memory to spare for long enough to give some
    /// back.
    fn spared(&self) -> bool;

    /// How much to give back this interval, if anything.
    fn due(&mut self, now: Timestamp, pressure: Pressure) -> Option<NonZeroU64>;

    /// The guest settled after a resize, or after an attempt at one.
    fn rested(&mut self, now: Timestamp);

    /// The workload came back under its ceiling after exhausting it.
    fn recovered(&mut self);
}

#[enum_dispatch(Release)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseStrategy {
    Titrate(Titrate),
}

impl From<ReleaseConfig> for ReleaseStrategy {
    fn from(config: ReleaseConfig) -> Self {
        match config {
            ReleaseConfig::Titrate(config) => Self::Titrate(Titrate::new(config)),
        }
    }
}
