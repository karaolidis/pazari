use std::time::Duration;

/// A retry delay that doubles up to a cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    current: Duration,
}

impl Backoff {
    #[must_use]
    pub const fn new(min: Duration, max: Duration) -> Self {
        Self {
            min,
            max,
            current: min,
        }
    }

    pub const fn reset(&mut self) {
        self.current = self.min;
    }

    pub fn delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = self.current.saturating_mul(2).min(self.max);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_millis(500);
    const MAX: Duration = Duration::from_secs(30);

    #[test]
    fn the_delay_climbs_until_it_reaches_the_cap() {
        let mut backoff = Backoff::new(MIN, MAX);

        assert!(backoff.delay() <= MIN, "the first delay is over the floor");

        for _ in 0..8 {
            backoff.delay();
        }

        assert!(
            backoff.delay() >= MAX / 2,
            "the delay never reached the cap"
        );
    }

    #[test]
    fn every_delay_is_twice_the_one_before_it() {
        let mut backoff = Backoff::new(MIN, Duration::from_secs(4));

        assert_eq!(
            [
                backoff.delay(),
                backoff.delay(),
                backoff.delay(),
                backoff.delay(),
                backoff.delay(),
            ],
            [
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(4),
            ]
        );
    }

    #[test]
    fn a_handshake_puts_the_next_delay_back_at_the_floor() {
        let mut backoff = Backoff::new(MIN, MAX);

        for _ in 0..8 {
            backoff.delay();
        }

        backoff.reset();

        assert!(
            backoff.delay() <= MIN,
            "the delay did not go back to the floor"
        );
    }

    #[test]
    fn a_floor_that_is_already_the_cap_never_grows() {
        let mut backoff = Backoff::new(MAX, MAX);

        for _ in 0..8 {
            assert!(backoff.delay() <= MAX);
        }
    }
}
