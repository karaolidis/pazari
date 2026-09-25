use std::num::NonZeroU64;

use pazari_types::{Deadline, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GrowScale(NonZeroU64);

impl GrowScale {
    pub const ONE: Self = Self(NonZeroU64::MIN);

    const TWO: NonZeroU64 = NonZeroU64::new(2).expect("two is not zero");
    const CAP: NonZeroU64 = NonZeroU64::new(64).expect("the cap is not zero");

    #[must_use]
    pub const fn doubled(self) -> Self {
        let next = self.0.saturating_mul(Self::TWO);

        Self(if next.get() > Self::CAP.get() {
            Self::CAP
        } else {
            next
        })
    }

    pub const fn of(self, step: NonZeroU64) -> u64 {
        step.get().saturating_mul(self.0.get())
    }

    pub const fn factor(self) -> u64 {
        self.0.get()
    }
}

impl Default for GrowScale {
    fn default() -> Self {
        Self::ONE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Escalation {
    scale: GrowScale,
    until: Option<Deadline>,
}

impl Escalation {
    pub const NONE: Self = Self {
        scale: GrowScale::ONE,
        until: None,
    };

    #[must_use]
    pub const fn settled(scale: GrowScale, until: Deadline) -> Self {
        Self {
            scale,
            until: Some(until),
        }
    }

    #[must_use]
    pub const fn braked(self, now: Timestamp) -> GrowScale {
        match self.until {
            Some(until) if !until.reached(now) => self.scale.doubled(),
            _ => GrowScale::ONE,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const START: Timestamp = Timestamp::from_millis(0);
    const INSIDE: Timestamp = Timestamp::from_millis(5_000);
    const OUTSIDE: Timestamp = Timestamp::from_millis(15_000);

    fn window() -> Deadline {
        Deadline::after(START, Duration::from_secs(10))
    }

    #[test]
    fn a_scale_doubles_to_its_cap_and_stops() {
        let mut scale = GrowScale::ONE;

        for expected in [2, 4, 8, 16, 32, 64, 64] {
            scale = scale.doubled();

            assert_eq!(scale.factor(), expected);
        }
    }

    #[test]
    fn a_scale_multiplies_the_configured_step() {
        let step = NonZeroU64::new(512).unwrap();

        assert_eq!(GrowScale::ONE.of(step), 512);
        assert_eq!(GrowScale::ONE.doubled().of(step), 1024);
    }

    #[test]
    fn a_brake_inside_the_window_doubles_and_one_outside_starts_over() {
        let escalation = Escalation::settled(GrowScale::ONE, window());

        assert_eq!(escalation.braked(INSIDE).factor(), 2);
        assert_eq!(escalation.braked(OUTSIDE), GrowScale::ONE);
    }

    #[test]
    fn a_brake_before_any_grow_has_settled_asks_for_one_step() {
        assert_eq!(Escalation::NONE.braked(START), GrowScale::ONE);
    }

    #[test]
    fn each_settled_grow_doubles_from_where_the_last_one_left_off() {
        let once = Escalation::settled(GrowScale::ONE, window());
        let twice = Escalation::settled(once.braked(INSIDE), window());

        assert_eq!(twice.braked(INSIDE).factor(), 4);
    }
}
