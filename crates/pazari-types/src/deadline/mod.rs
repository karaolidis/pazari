use std::time::Duration;

use crate::time::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Deadline(Timestamp);

impl Deadline {
    #[must_use]
    pub fn after(now: Timestamp, delay: Duration) -> Self {
        Self(now.saturating_add(delay))
    }

    #[must_use]
    pub const fn reached(self, now: Timestamp) -> bool {
        now.as_millis() >= self.0.as_millis()
    }

    #[must_use]
    pub fn furthest(self, other: Option<Self>) -> Self {
        match other {
            Some(other) if other > self => other,
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp::from_millis(1_000);

    fn at(millis: u64) -> Deadline {
        Deadline::after(Timestamp::from_millis(millis), Duration::ZERO)
    }

    #[test]
    fn a_deadline_is_reached_on_its_own_millisecond() {
        let deadline = Deadline::after(NOW, Duration::from_secs(30));

        assert!(!deadline.reached(Timestamp::from_millis(30_999)));
        assert!(deadline.reached(Timestamp::from_millis(31_000)));
        assert!(deadline.reached(Timestamp::from_millis(31_001)));
    }

    #[test]
    fn arming_over_a_standing_deadline_keeps_the_later_one() {
        assert_eq!(at(500).furthest(Some(at(900))), at(900));
        assert_eq!(at(900).furthest(Some(at(500))), at(900));
        assert_eq!(at(500).furthest(None), at(500));
    }
}
