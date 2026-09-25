use std::num::{NonZeroU32, NonZeroU64};

use pazari_types::{Delay, Interval, Swappiness};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExhaustionMode {
    Stall { timeout: Interval },

    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// `S` in `memory.max = P - S`.
    pub service_reserve: u64,

    /// `R` in `memory.high = P - S - R`.
    pub burst_reserve: NonZeroU64,

    pub grow_step: NonZeroU64,
    pub deny_backoff: Interval,
    pub settle_grace: Interval,

    pub escalate_within: Interval,

    pub release_intervals: NonZeroU32,

    pub shrink_step: NonZeroU64,
    pub shrink_cooldown: Delay,

    pub reclaim_swappiness: Option<Swappiness>,

    pub exhaustion: ExhaustionMode,

    pub tick_interval: Interval,
}
