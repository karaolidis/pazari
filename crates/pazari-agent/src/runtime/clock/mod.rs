use std::time::Instant;

use pazari_types::Timestamp;

/// The agent's own clock. Milliseconds since it started.
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
    use std::{thread, time::Duration};

    use super::*;

    #[test]
    fn the_clock_starts_at_the_agent_and_only_moves_forward() {
        let clock = Clock::start();

        let first = clock.now();
        thread::sleep(Duration::from_millis(5));
        let second = clock.now();

        assert!(first.as_millis() < 100, "started at {first}");
        assert!(second >= first, "{second} is before {first}");
        assert!(second.as_millis() >= 5, "only reached {second}");
    }
}
