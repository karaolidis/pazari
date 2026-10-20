use std::num::{NonZeroU32, NonZeroU64};

use pazari_types::{Delay, Interval};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExhaustionMode {
    Stall { timeout: Interval },

    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleMode {
    Grow,

    Reclaim { amount: NonZeroU64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrowConfig {
    Exponential(ExponentialConfig),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExponentialConfig {
    pub step: NonZeroU64,
    pub escalate_within: Interval,
    pub cap: NonZeroU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseConfig {
    Titrate(TitrateConfig),
    Sweep(SweepConfig),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitrateConfig {
    pub step: NonZeroU64,
    pub after: NonZeroU32,
    pub cooldown: Delay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepConfig {
    pub min: NonZeroU64,
    pub hold: Delay,
    pub cooldown: Delay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    pub service_reserve: u64,

    pub burst_reserve: NonZeroU64,

    pub throttle: ThrottleMode,
    pub grow: GrowConfig,
    pub release: ReleaseConfig,

    pub settle_grace: Interval,
    pub deny_backoff: Interval,
    pub recover_after: NonZeroU32,

    pub exhaustion: ExhaustionMode,
}
