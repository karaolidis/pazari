use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{bounded::Bounded, error::RangeError};

/// A proportion of something, in `(0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Fraction(f64);

impl Fraction {
    pub const MAX: f64 = 1.0;

    pub fn try_new(value: f64) -> Result<Self, RangeError> {
        <Self as Bounded>::try_new(value)
    }

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Bounded for Fraction {
    type Raw = f64;

    const NAME: &'static str = "Fraction";
    const BOUNDS: &'static str = "(0, 1]";

    fn contains(value: f64) -> bool {
        value.is_finite() && value > 0.0 && value <= Self::MAX
    }

    fn wrap(value: f64) -> Self {
        Self(value)
    }

    fn get(self) -> f64 {
        self.0
    }
}

impl fmt::Display for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<f64> for Fraction {
    type Error = RangeError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<Fraction> for f64 {
    fn from(value: Fraction) -> Self {
        value.0
    }
}

/// A pressure-stall percentage, in `(0, 100]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Psi(f64);

impl Psi {
    pub const MAX: f64 = 100.0;

    pub fn try_new(value: f64) -> Result<Self, RangeError> {
        <Self as Bounded>::try_new(value)
    }

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Bounded for Psi {
    type Raw = f64;

    const NAME: &'static str = "Psi";
    const BOUNDS: &'static str = "(0, 100]";

    fn contains(value: f64) -> bool {
        value.is_finite() && value > 0.0 && value <= Self::MAX
    }

    fn wrap(value: f64) -> Self {
        Self(value)
    }

    fn get(self) -> f64 {
        self.0
    }
}

impl fmt::Display for Psi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<f64> for Psi {
    type Error = RangeError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<Psi> for f64 {
    fn from(value: Psi) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUST_OVER_ONE: f64 = 1.0 + f64::EPSILON;

    const JUST_OVER_ONE_HUNDRED: f64 = f64::from_bits(100.0_f64.to_bits() + 1);

    #[test]
    fn a_fraction_spans_zero_exclusive_to_one() {
        assert!(Fraction::try_new(0.0).is_err());
        assert!(Fraction::try_new(-0.0).is_err());
        assert!(Fraction::try_new(-0.5).is_err());
        assert_eq!(
            Fraction::try_new(f64::MIN_POSITIVE).map(Fraction::get),
            Ok(f64::MIN_POSITIVE)
        );
        assert_eq!(Fraction::try_new(1.0).map(Fraction::get), Ok(1.0));
        assert!(Fraction::try_new(JUST_OVER_ONE).is_err());
    }

    #[test]
    fn a_psi_spans_zero_exclusive_to_one_hundred() {
        assert!(Psi::try_new(0.0).is_err());
        assert!(Psi::try_new(-1.0).is_err());
        assert_eq!(
            Psi::try_new(f64::MIN_POSITIVE).map(Psi::get),
            Ok(f64::MIN_POSITIVE)
        );
        assert_eq!(Psi::try_new(100.0).map(Psi::get), Ok(100.0));
        assert!(Psi::try_new(JUST_OVER_ONE_HUNDRED).is_err());
    }

    #[test]
    fn no_ratio_admits_a_non_finite() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Fraction::try_new(value).is_err(), "Fraction took {value}");
            assert!(Psi::try_new(value).is_err(), "Psi took {value}");
        }
    }

    #[test]
    fn a_ratio_displays_as_its_number() {
        assert_eq!(
            Fraction::try_new(0.25)
                .expect("a quarter is a fraction")
                .to_string(),
            "0.25"
        );
        assert_eq!(Psi::try_new(5.0).expect("5% is a psi").to_string(), "5");
    }

    #[test]
    fn a_ratio_serialises_as_its_number() {
        let fraction = Fraction::try_new(0.5).expect("a half is a fraction");

        assert_eq!(
            serde_json::to_string(&fraction).expect("f64 serialises"),
            "0.5"
        );
    }

    #[test]
    fn deserialisation_validates() {
        assert!(serde_json::from_str::<Fraction>("0").is_err());
        assert!(serde_json::from_str::<Fraction>("1.5").is_err());
        assert!(serde_json::from_str::<Psi>("100").is_ok());
        assert!(serde_json::from_str::<Psi>("101").is_err());
    }
}
