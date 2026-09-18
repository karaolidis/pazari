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
