use std::num::NonZeroU64;

use pazari_types::{Deadline, Scale, Timestamp};

use crate::engine::{config::ExponentialConfig, grow::Grow};

/// One step at first, doubled for each brake that follows soon after the
/// last grow, up to a cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exponential {
    config: ExponentialConfig,
    scale: Scale,
    escalate_until: Option<Deadline>,
}

impl Exponential {
    const CAP: NonZeroU64 = NonZeroU64::new(64).expect("the cap is not zero");

    pub const fn new(config: ExponentialConfig) -> Self {
        Self {
            config,
            scale: Scale::new(Self::CAP),
            escalate_until: None,
        }
    }
}

impl Grow for Exponential {
    fn step(&self) -> NonZeroU64 {
        self.scale.of(self.config.step)
    }

    fn braked(&mut self, now: Timestamp) {
        self.scale = match self.escalate_until {
            Some(until) if !until.reached(now) => self.scale.doubled(),
            _ => self.scale.reset(),
        };
        self.escalate_until = None;
    }

    fn settled(&mut self, now: Timestamp) {
        self.escalate_until = Some(Deadline::after(now, self.config.escalate_within.get()));
    }

    fn reset(&mut self) {
        *self = Self::new(self.config);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pazari_types::Interval;

    use super::*;

    const MIB: u64 = 1 << 20;

    const START: Timestamp = Timestamp::from_millis(0);
    const INSIDE: Timestamp = Timestamp::from_millis(5_000);
    const OUTSIDE: Timestamp = Timestamp::from_millis(15_000);

    fn exponential() -> Exponential {
        Exponential::new(ExponentialConfig {
            step: NonZeroU64::new(512 * MIB).expect("a positive step"),
            escalate_within: Interval::try_new(Duration::from_secs(10))
                .expect("a positive interval"),
        })
    }

    fn settled() -> Exponential {
        let mut grow = exponential();
        grow.settled(START);
        grow
    }

    #[test]
    fn escalation_stops_at_sixty_four_steps() {
        let mut grow = settled();

        for _ in 0..8 {
            grow.braked(INSIDE);
            grow.settled(INSIDE);
        }
        grow.braked(INSIDE);

        assert_eq!(grow.step().get(), 64 * 512 * MIB);
    }

    #[test]
    fn a_brake_inside_the_window_doubles_and_one_outside_starts_over() {
        let mut inside = settled();
        inside.braked(INSIDE);
        assert_eq!(inside.step().get(), 1024 * MIB);

        let mut outside = settled();
        outside.braked(OUTSIDE);
        assert_eq!(outside.step().get(), 512 * MIB);
    }

    #[test]
    fn a_brake_before_any_grow_has_settled_asks_for_one_step() {
        let mut grow = exponential();
        grow.braked(START);

        assert_eq!(grow.step().get(), 512 * MIB);
    }

    #[test]
    fn each_settled_grow_doubles_from_where_the_last_one_left_off() {
        let mut grow = settled();
        grow.braked(INSIDE);
        grow.settled(INSIDE);
        grow.braked(INSIDE);

        assert_eq!(grow.step().get(), 2048 * MIB);
    }

    #[test]
    fn a_brake_without_a_grow_in_between_starts_over() {
        let mut grow = settled();
        grow.braked(INSIDE);
        grow.braked(INSIDE);

        assert_eq!(grow.step().get(), 512 * MIB);
    }

    #[test]
    fn a_reset_forgets_the_escalation() {
        let mut grow = settled();
        grow.braked(INSIDE);
        grow.settled(INSIDE);
        grow.reset();

        assert_eq!(grow, exponential());
        grow.braked(INSIDE);
        assert_eq!(grow.step().get(), 512 * MIB);
    }
}
