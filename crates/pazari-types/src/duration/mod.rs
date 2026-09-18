use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{bounded::Bounded, error::RangeError};

pub const MAX_DURATION: Duration = Duration::from_hours(24);

/// A recurring, strictly positive period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Duration", into = "Duration")]
pub struct Interval(Duration);

impl Interval {
    pub const MAX: Duration = MAX_DURATION;

    pub fn try_new(value: Duration) -> Result<Self, RangeError> {
        <Self as Bounded>::try_new(value)
    }

    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl Bounded for Interval {
    type Raw = Duration;

    const NAME: &'static str = "Interval";
    const BOUNDS: &'static str = "(0s, 24h]";

    fn contains(value: Duration) -> bool {
        !value.is_zero() && value <= Self::MAX
    }

    fn wrap(value: Duration) -> Self {
        Self(value)
    }

    fn get(self) -> Duration {
        self.0
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl TryFrom<Duration> for Interval {
    type Error = RangeError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<Interval> for Duration {
    fn from(value: Interval) -> Self {
        value.0
    }
}

/// A one-shot wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Duration", into = "Duration")]
pub struct Delay(Duration);

impl Delay {
    pub const ZERO: Self = Self(Duration::ZERO);
    pub const MAX: Duration = MAX_DURATION;

    pub fn try_new(value: Duration) -> Result<Self, RangeError> {
        <Self as Bounded>::try_new(value)
    }

    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

impl Bounded for Delay {
    type Raw = Duration;

    const NAME: &'static str = "Delay";
    const BOUNDS: &'static str = "[0s, 24h]";

    fn contains(value: Duration) -> bool {
        value <= Self::MAX
    }

    fn wrap(value: Duration) -> Self {
        Self(value)
    }

    fn get(self) -> Duration {
        self.0
    }
}

impl fmt::Display for Delay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl TryFrom<Duration> for Delay {
    type Error = RangeError;

    fn try_from(value: Duration) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<Delay> for Duration {
    fn from(value: Delay) -> Self {
        value.0
    }
}
