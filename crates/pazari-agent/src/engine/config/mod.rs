use std::num::{NonZeroU32, NonZeroU64};

use pazari_types::{Delay, Interval, Swappiness};

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

impl ThrottleMode {
    /// How much to reclaim before asking the host, given the guest's slack.
    #[must_use]
    pub fn reclaim(self, slack: Option<u64>) -> Option<NonZeroU64> {
        match self {
            Self::Grow => None,
            Self::Reclaim { amount } => slack
                .is_some_and(|slack| slack >= amount.get())
                .then_some(amount),
        }
    }
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
    Sweep(SweepConfig),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitrateConfig {
    pub step: NonZeroU64,
    pub intervals: NonZeroU32,
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

    pub deny_backoff: Interval,
    pub settle_grace: Interval,
    pub recovery_intervals: NonZeroU32,

    pub reclaim_swappiness: Option<Swappiness>,

    pub exhaustion: ExhaustionMode,

    pub tick_interval: Interval,
}
