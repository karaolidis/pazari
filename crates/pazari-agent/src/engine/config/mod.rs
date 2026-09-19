use std::{
    fmt,
    num::{NonZeroU32, NonZeroU64},
};

use pazari_types::{Bounded, Delay, Fraction, Interval, Psi, RangeError, Swappiness};

/// The grow/shrink deadband.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct IdleThreshold(Fraction);

impl IdleThreshold {
    pub const MAX: f64 = 0.9;

    pub fn try_new(value: f64) -> Result<Self, RangeError> {
        <Self as Bounded>::try_new(value)
    }

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0.get()
    }
}

impl Bounded for IdleThreshold {
    type Raw = f64;

    const NAME: &'static str = "IdleThreshold";
    const BOUNDS: &'static str = "(0, 0.9]";

    fn contains(value: f64) -> bool {
        Fraction::contains(value) && value <= Self::MAX
    }

    fn wrap(value: f64) -> Self {
        Self(Fraction::wrap(value))
    }

    fn get(self) -> f64 {
        self.0.get()
    }
}

impl fmt::Display for IdleThreshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<f64> for IdleThreshold {
    type Error = RangeError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExhaustionMode {
    Stall { timeout: Interval },

    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineConfig {
    /// `S` in `memory.max = P - S`.
    pub service_reserve: u64,

    /// `R` in `memory.high = P - S - R`.
    pub burst_reserve: NonZeroU64,

    /// The part of `R` withheld while a grant is in flight.
    pub open_guard: NonZeroU64,

    pub grow_step: NonZeroU64,
    pub deny_backoff: Interval,
    pub settle_grace: Interval,

    pub idle_threshold: IdleThreshold,
    pub idle_intervals: NonZeroU32,

    pub shrink_step: NonZeroU64,
    pub shrink_cooldown: Delay,

    pub reclaim_swappiness: Option<Swappiness>,

    pub titrate_step: NonZeroU64,
    pub titrate_interval: Interval,
    pub titrate_psi_threshold: Psi,

    pub exhaustion: ExhaustionMode,

    pub tick_interval: Interval,
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn the_idle_threshold_stops_below_full_utilisation() {
        assert_eq!(
            IdleThreshold::try_from(0.5).map(IdleThreshold::get),
            Ok(0.5)
        );
        assert_eq!(
            IdleThreshold::try_from(IdleThreshold::MAX).map(IdleThreshold::get),
            Ok(IdleThreshold::MAX)
        );
        assert!(IdleThreshold::try_from(0.91).is_err());
        assert!(IdleThreshold::try_from(1.0).is_err());
        assert!(IdleThreshold::try_from(0.0).is_err());
        assert!(IdleThreshold::try_from(f64::NAN).is_err());
    }

    proptest! {
        #[test]
        fn the_idle_threshold_admits_only_its_deadband(
            value in prop_oneof![Just(f64::NAN), Just(f64::INFINITY), -1.0_f64..2.0],
        ) {
            if let Ok(threshold) = IdleThreshold::try_from(value) {
                prop_assert!(threshold.get() > 0.0);
                prop_assert!(threshold.get() <= IdleThreshold::MAX);
            }
        }
    }
}
