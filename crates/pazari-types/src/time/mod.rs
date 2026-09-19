use std::{
    fmt,
    time::{Duration, Instant},
};

/// Milliseconds since a state machine started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(u64);

impl Timestamp {
    pub const START: Self = Self(0);

    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn saturating_add(self, after: Duration) -> Self {
        let millis = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
        Self(self.0.saturating_add(millis))
    }

    #[must_use]
    pub const fn saturating_duration_since(self, earlier: Self) -> Duration {
        Duration::from_millis(self.0.saturating_sub(earlier.0))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

/// The process's own monotonic clock.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    start: Instant,
}

impl Clock {
    #[must_use]
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    #[must_use]
    pub fn now(self) -> Timestamp {
        let millis = self.start.elapsed().as_millis();

        Timestamp::from_millis(u64::try_from(millis).unwrap_or(u64::MAX))
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::start()
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn timestamps_saturate_rather_than_wrap() {
        let last = Timestamp::from_millis(u64::MAX);

        assert_eq!(Timestamp::START.as_millis(), 0);
        assert_eq!(
            Timestamp::START.saturating_add(Duration::from_secs(5)),
            Timestamp::from_millis(5_000)
        );
        assert_eq!(last.saturating_add(Duration::from_secs(1)), last);
        assert_eq!(Timestamp::START.saturating_add(Duration::MAX), last);
        assert_eq!(
            Timestamp::from_millis(2_000).saturating_duration_since(Timestamp::from_millis(500)),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            Timestamp::START.saturating_duration_since(Timestamp::from_millis(5_000)),
            Duration::ZERO
        );
    }

    #[test]
    fn a_timestamp_displays_its_milliseconds() {
        assert_eq!(Timestamp::from_millis(1_500).to_string(), "1500ms");
        assert_eq!(Timestamp::START.to_string(), "0ms");
    }

    #[test]
    fn the_clock_starts_with_the_process_and_only_moves_forward() {
        let clock = Clock::start();

        let first = clock.now();
        thread::sleep(Duration::from_millis(5));
        let second = clock.now();

        assert!(first.as_millis() < 100, "started at {first}");
        assert!(second >= first, "{second} is before {first}");
        assert!(second.as_millis() >= 5, "only reached {second}");
    }
}
