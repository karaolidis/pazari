use std::{fmt, time::Duration};

/// Milliseconds since the agent started.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testing::at;

    #[test]
    fn timestamps_saturate_rather_than_wrap() {
        let last = Timestamp::from_millis(u64::MAX);

        assert_eq!(Timestamp::START.as_millis(), 0);
        assert_eq!(
            Timestamp::START.saturating_add(Duration::from_secs(5)),
            at(5)
        );
        assert_eq!(last.saturating_add(Duration::from_secs(1)), last);
        assert_eq!(Timestamp::START.saturating_add(Duration::MAX), last);
        assert_eq!(
            at(2).saturating_duration_since(Timestamp::from_millis(500)),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            Timestamp::START.saturating_duration_since(at(5)),
            Duration::ZERO
        );
    }
}
