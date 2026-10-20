use std::num::NonZeroU64;

/// Times the workload reached `memory.max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Breaches(NonZeroU64);

impl Breaches {
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// The `memory.events` counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub high: u64,
    pub max: u64,
}

/// What one look at `memory.events` added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Events {
    pub throttled: bool,
    pub breaches: Option<Breaches>,
}

/// What the workload has run into since the last interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pressure {
    pub throttled: bool,
    pub breached: bool,
}

impl Pressure {
    /// Whether the workload ran into neither limit.
    pub const fn calm(self) -> bool {
        !self.throttled && !self.breached
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CounterTracker {
    seen: Option<Counters>,
    since: Pressure,
}

impl CounterTracker {
    pub fn observe(&mut self, high: u64, max: u64, wake: bool) -> Events {
        let events = Events {
            throttled: self
                .seen
                .map_or(wake && high > 0, |previous| high > previous.high),
            breaches: self
                .seen
                .and_then(|previous| NonZeroU64::new(max.saturating_sub(previous.max)))
                .map(Breaches),
        };

        self.seen = Some(Counters { high, max });
        self.since.throttled |= events.throttled;
        self.since.breached |= events.breaches.is_some();

        events
    }

    pub const fn take(&mut self) -> Pressure {
        let since = self.since;
        self.since = Pressure {
            throttled: false,
            breached: false,
        };
        since
    }

    pub fn seen(self) -> Counters {
        self.seen.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_reading_of_a_tick_cannot_tell_a_throttle_from_a_backlog() {
        let mut tracker = CounterTracker::default();

        assert!(!tracker.observe(7, 0, false).throttled);
    }

    #[test]
    fn the_first_reading_of_a_wake_trusts_a_positive_count() {
        let mut tracker = CounterTracker::default();

        assert!(tracker.observe(7, 0, true).throttled);
        assert!(!CounterTracker::default().observe(0, 0, true).throttled);
    }

    #[test]
    fn only_a_rising_high_count_is_a_throttle() {
        let mut tracker = CounterTracker::default();

        tracker.observe(1, 0, false);
        assert!(!tracker.observe(1, 0, false).throttled);
        assert!(tracker.observe(2, 0, false).throttled);
    }

    #[test]
    fn breaches_count_how_far_the_max_counter_moved() {
        let mut tracker = CounterTracker::default();

        tracker.observe(0, 4, false);
        assert_eq!(tracker.observe(0, 4, false).breaches, None);
        assert_eq!(
            tracker.observe(0, 7, false).breaches.map(Breaches::get),
            Some(3)
        );
    }

    #[test]
    fn pressure_gathers_until_an_interval_takes_it() {
        let mut tracker = CounterTracker::default();

        tracker.observe(1, 0, false);
        tracker.observe(2, 1, false);
        tracker.observe(2, 1, false);

        assert_eq!(
            tracker.take(),
            Pressure {
                throttled: true,
                breached: true,
            }
        );
        assert_eq!(tracker.take(), Pressure::default());
    }
}
