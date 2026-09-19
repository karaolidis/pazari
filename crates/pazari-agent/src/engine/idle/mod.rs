use std::{num::NonZeroU32, time::Duration};

use pazari_types::Timestamp;

use crate::engine::config::IdleThreshold;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IdleTracker {
    ticks: u32,
    throttled: bool,
    cooldown_until: Option<Timestamp>,
}

impl IdleTracker {
    pub const fn throttled(self) -> bool {
        self.throttled
    }

    pub const fn note_throttle(&mut self) {
        self.throttled = true;
        self.ticks = 0;
    }

    pub const fn reset(&mut self) {
        self.ticks = 0;
    }

    pub const fn take_throttled(&mut self) -> bool {
        let throttled = self.throttled;
        self.throttled = false;
        throttled
    }

    pub fn arm_cooldown(&mut self, now: Timestamp, delay: Duration) {
        self.cooldown_until = Some(now.saturating_add(delay));
    }

    pub fn cooling(self, now: Timestamp) -> bool {
        self.cooldown_until.is_some_and(|until| now < until)
    }

    pub fn note(
        &mut self,
        throttled: bool,
        usage: u64,
        budget: u64,
        threshold: IdleThreshold,
        intervals: NonZeroU32,
    ) -> bool {
        if !Self::under_deadband(throttled, usage, budget, threshold) {
            self.ticks = 0;
            return false;
        }

        self.ticks = self.ticks.saturating_add(1);
        self.ticks >= intervals.get()
    }

    fn under_deadband(throttled: bool, usage: u64, budget: u64, threshold: IdleThreshold) -> bool {
        #[expect(
            clippy::cast_precision_loss,
            reason = "byte counts stay far below the f64 mantissa"
        )]
        let utilisation = usage as f64 / budget as f64;

        !throttled && utilisation < threshold.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: u64 = 1000;

    fn threshold() -> IdleThreshold {
        IdleThreshold::try_from(0.5).unwrap()
    }

    fn intervals(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap()
    }

    fn quiet(tracker: &mut IdleTracker) -> bool {
        tracker.note(false, 100, BUDGET, threshold(), intervals(3))
    }

    #[test]
    fn a_streak_has_to_reach_the_configured_length() {
        let mut tracker = IdleTracker::default();

        assert!(!quiet(&mut tracker));
        assert!(!quiet(&mut tracker));
        assert!(quiet(&mut tracker));
        assert!(quiet(&mut tracker));
    }

    #[test]
    fn usage_on_the_deadband_is_not_idle() {
        let mut tracker = IdleTracker::default();

        assert!(!tracker.note(false, 500, BUDGET, threshold(), intervals(1)));
        assert!(tracker.note(false, 499, BUDGET, threshold(), intervals(1)));
    }

    #[test]
    fn a_throttled_interval_is_never_idle() {
        let mut tracker = IdleTracker::default();

        assert!(!quiet(&mut tracker));
        assert!(!quiet(&mut tracker));
        assert!(!tracker.note(true, 0, BUDGET, threshold(), intervals(3)));
        assert!(!quiet(&mut tracker));
    }

    #[test]
    fn a_budget_of_zero_is_not_idle() {
        let mut tracker = IdleTracker::default();

        assert!(!tracker.note(false, 0, 0, threshold(), intervals(1)));
        assert!(!tracker.note(false, 100, 0, threshold(), intervals(1)));
    }

    #[test]
    fn the_throttle_flag_lasts_exactly_one_interval() {
        let mut tracker = IdleTracker::default();

        tracker.note_throttle();
        assert!(tracker.throttled());
        assert!(tracker.take_throttled());
        assert!(!tracker.throttled());
        assert!(!tracker.take_throttled());
    }

    #[test]
    fn a_throttle_drops_the_streak_on_the_spot() {
        let mut tracker = IdleTracker::default();

        assert!(!quiet(&mut tracker));
        assert!(!quiet(&mut tracker));
        tracker.note_throttle();
        assert!(!quiet(&mut tracker));
    }

    #[test]
    fn the_cooldown_runs_out_at_its_deadline() {
        let mut tracker = IdleTracker::default();

        assert!(!tracker.cooling(Timestamp::START));

        tracker.arm_cooldown(Timestamp::from_millis(1_000), Duration::from_secs(30));

        assert!(tracker.cooling(Timestamp::from_millis(30_999)));
        assert!(!tracker.cooling(Timestamp::from_millis(31_000)));
    }
}
