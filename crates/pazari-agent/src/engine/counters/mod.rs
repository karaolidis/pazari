use std::num::NonZeroU64;

/// The `memory.events` counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub high: u64,
    pub max: u64,
}

/// What one look at `memory.events` added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Events {
    pub throttled: bool,

    /// Times the workload reached `memory.max`.
    pub breaches: Option<NonZeroU64>,
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

/// What prompted a look at `memory.events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The periodic tick.
    Tick,

    /// The throttle watcher, which only wakes when the counters move.
    Wake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CounterTracker {
    seen: Option<Counters>,
    since: Pressure,
}

impl CounterTracker {
    pub fn observe(&mut self, high: u64, max: u64, source: Source) -> Events {
        let events = Events {
            throttled: self
                .seen
                .map_or(source == Source::Wake && high > 0, |previous| {
                    high > previous.high
                }),
            breaches: self
                .seen
                .and_then(|previous| NonZeroU64::new(max.saturating_sub(previous.max))),
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

    pub const fn seen(self) -> Option<Counters> {
        self.seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_reading_of_a_tick_cannot_tell_a_throttle_from_a_backlog() {
        let mut tracker = CounterTracker::default();

        assert!(!tracker.observe(7, 0, Source::Tick).throttled);
    }

    #[test]
    fn the_first_reading_of_a_wake_trusts_a_positive_count() {
        let mut tracker = CounterTracker::default();

        assert!(tracker.observe(7, 0, Source::Wake).throttled);
        assert!(
            !CounterTracker::default()
                .observe(0, 0, Source::Wake)
                .throttled
        );
    }

    #[test]
    fn only_a_rising_high_count_is_a_throttle() {
        let mut tracker = CounterTracker::default();

        tracker.observe(1, 0, Source::Tick);
        assert!(!tracker.observe(1, 0, Source::Tick).throttled);
        assert!(tracker.observe(2, 0, Source::Tick).throttled);
    }

    #[test]
    fn breaches_count_how_far_the_max_counter_moved() {
        let mut tracker = CounterTracker::default();

        tracker.observe(0, 4, Source::Tick);
        assert_eq!(tracker.observe(0, 4, Source::Tick).breaches, None);
        assert_eq!(
            tracker.observe(0, 7, Source::Tick).breaches,
            NonZeroU64::new(3)
        );
    }

    #[test]
    fn pressure_gathers_until_an_interval_takes_it() {
        let mut tracker = CounterTracker::default();

        tracker.observe(1, 0, Source::Tick);
        tracker.observe(2, 1, Source::Tick);
        tracker.observe(2, 1, Source::Tick);

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
