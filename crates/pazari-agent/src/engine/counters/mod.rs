#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub high: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CounterTracker {
    seen: Option<Counters>,
}

impl CounterTracker {
    pub fn observe(&mut self, high: u64, max: u64, wake: bool) -> bool {
        let throttled = self
            .seen
            .map_or(wake && high > 0, |previous| high > previous.high);

        self.seen = Some(Counters { high, max });

        throttled
    }

    pub fn seen(self) -> Counters {
        self.seen.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_reading_is_only_a_baseline() {
        let mut tracker = CounterTracker::default();

        assert!(!tracker.observe(7, 3, false));
        assert_eq!(tracker.seen(), Counters { high: 7, max: 3 });
    }

    #[test]
    fn a_first_reading_on_a_wake_is_a_throttle() {
        let mut tracker = CounterTracker::default();

        assert!(tracker.observe(1, 0, true));
    }

    #[test]
    fn a_wake_with_nothing_counted_is_not_a_throttle() {
        let mut tracker = CounterTracker::default();

        assert!(!tracker.observe(0, 0, true));
    }

    #[test]
    fn only_an_advance_is_a_throttle() {
        let mut tracker = CounterTracker::default();

        tracker.observe(4, 0, false);

        assert!(!tracker.observe(4, 0, false));
        assert!(!tracker.observe(4, 0, true));
        assert!(tracker.observe(5, 0, false));
        assert!(tracker.observe(9, 0, true));
    }

    #[test]
    fn a_reset_counter_is_not_a_throttle() {
        let mut tracker = CounterTracker::default();

        tracker.observe(7, 2, false);

        assert!(!tracker.observe(2, 0, false));
        assert_eq!(tracker.seen(), Counters { high: 2, max: 0 });
    }

    #[test]
    fn nothing_seen_reads_as_zero() {
        assert_eq!(CounterTracker::default().seen(), Counters::default());
    }
}
