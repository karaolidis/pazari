use std::num::{NonZeroU32, NonZeroU64};

use pazari_types::{Delay, Interval, Swappiness};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExhaustionMode {
    Stall { timeout: Interval },

    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrowConfig {
    Exponential(ExponentialConfig),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExponentialConfig {
    pub step: NonZeroU64,
    pub escalate_within: Interval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseConfig {
    Titrate(TitrateConfig),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitrateConfig {
    pub step: NonZeroU64,
    pub intervals: NonZeroU32,
    pub cooldown: Delay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    pub service_reserve: u64,

    pub burst_reserve: NonZeroU64,

    pub grow: GrowConfig,
    pub release: ReleaseConfig,

    pub deny_backoff: Interval,
    pub settle_grace: Interval,
    pub recovery_intervals: NonZeroU32,

    pub reclaim_swappiness: Option<Swappiness>,

    pub exhaustion: ExhaustionMode,

    pub tick_interval: Interval,
}
