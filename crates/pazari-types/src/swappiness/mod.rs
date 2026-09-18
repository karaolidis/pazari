use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{bounded::Bounded, error::RangeError};

/// A `swappiness=` argument for `memory.reclaim`.
///
/// The kernel accepts `0..=200` and rejects anything else with `EINVAL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Swappiness(u8);

impl Swappiness {
    pub const NONE: Self = Self(0);
    pub const MAX: u8 = 200;

    pub fn try_new(value: u8) -> Result<Self, RangeError> {
        <Self as Bounded>::try_new(value)
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Bounded for Swappiness {
    type Raw = u8;

    const NAME: &'static str = "Swappiness";
    const BOUNDS: &'static str = "[0, 200]";

    fn contains(value: u8) -> bool {
        value <= Self::MAX
    }

    fn wrap(value: u8) -> Self {
        Self(value)
    }

    fn get(self) -> u8 {
        self.0
    }
}

impl fmt::Display for Swappiness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<u8> for Swappiness {
    type Error = RangeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<Swappiness> for u8 {
    fn from(value: Swappiness) -> Self {
        value.0
    }
}
