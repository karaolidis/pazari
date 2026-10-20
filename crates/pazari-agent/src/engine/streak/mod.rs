use std::num::NonZeroU32;

/// Consecutive intervals a condition has held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Streak(u32);

impl Streak {
    pub const BROKEN: Self = Self(0);

    #[must_use]
    pub const fn then(self, held: bool) -> Self {
        if held {
            Self(self.0.saturating_add(1))
        } else {
            Self::BROKEN
        }
    }

    pub const fn reached(self, intervals: NonZeroU32) -> bool {
        self.0 >= intervals.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testing::count;

    #[test]
    fn a_streak_has_to_reach_the_configured_length() {
        let mut streak = Streak::BROKEN;

        for expected in [false, false, true, true] {
            streak = streak.then(true);

            assert_eq!(streak.reached(count(3)), expected);
        }
    }

    #[test]
    fn one_interval_that_does_not_hold_starts_the_count_over() {
        let streak = Streak::BROKEN.then(true).then(true).then(false);

        assert_eq!(streak, Streak::BROKEN);
        assert!(!streak.then(true).reached(count(3)));
    }
}
