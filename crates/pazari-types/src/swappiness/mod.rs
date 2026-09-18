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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swappiness_spans_what_the_kernel_accepts() {
        assert_eq!(Swappiness::try_new(0).map(Swappiness::get), Ok(0));
        assert_eq!(Swappiness::NONE.get(), 0);
        assert_eq!(Swappiness::try_new(200).map(Swappiness::get), Ok(200));
        assert!(Swappiness::try_new(201).is_err());
        assert!(Swappiness::try_new(u8::MAX).is_err());
    }

    #[test]
    fn swappiness_displays_as_its_number() {
        assert_eq!(Swappiness::NONE.to_string(), "0");
    }

    #[test]
    fn swappiness_serialises_as_its_number() {
        let swappiness = Swappiness::try_new(60).expect("60 is a swappiness");

        assert_eq!(
            serde_json::to_string(&swappiness).expect("u8 serialises"),
            "60"
        );
    }

    #[test]
    fn deserialisation_validates() {
        assert!(serde_json::from_str::<Swappiness>("200").is_ok());
        assert!(serde_json::from_str::<Swappiness>("201").is_err());
    }
}
