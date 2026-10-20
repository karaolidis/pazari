use std::num::NonZeroU64;

use pazari_types::{Deadline, Scale, Timestamp};

use crate::engine::{config::ExponentialConfig, grow::Grow};

/// One step at first. Each brake that follows soon after the last grow doubles
/// it, up to `cap` steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exponential {
    config: ExponentialConfig,
    scale: Scale,
    escalate_until: Option<Deadline>,
}

impl Exponential {
    pub fn new(config: ExponentialConfig) -> Self {
        Self {
            config,
            scale: Scale::new(NonZeroU64::from(config.cap)),
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
    use super::*;
    use crate::engine::testing::{MIB, at, bytes, count, interval};

    const START: Timestamp = at(0);
    const INSIDE: Timestamp = at(5);
    const OUTSIDE: Timestamp = at(15);

    fn exponential() -> Exponential {
        Exponential::new(ExponentialConfig {
            step: bytes(512 * MIB),
            escalate_within: interval(10),
            cap: count(64),
        })
    }

    fn settled() -> Exponential {
        let mut grow = exponential();
        grow.settled(START);
        grow
    }

    #[test]
    fn escalation_stops_at_the_cap() {
        let mut grow = settled();

        for _ in 0..8 {
            grow.braked(INSIDE);
            grow.settled(INSIDE);
        }
        grow.braked(INSIDE);

        assert_eq!(grow.step().get(), 64 * 512 * MIB);
    }

    #[test]
    fn a_smaller_cap_stops_escalation_sooner() {
        let mut grow = Exponential::new(ExponentialConfig {
            cap: count(4),
            ..exponential().config
        });
        grow.settled(START);

        for _ in 0..4 {
            grow.braked(INSIDE);
            grow.settled(INSIDE);
        }

        assert_eq!(grow.step().get(), 4 * 512 * MIB);
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
