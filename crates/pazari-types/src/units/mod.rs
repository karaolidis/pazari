//! The bounded scalars pazari measures itself in.

pub mod duration;
pub mod ratio;
pub mod swappiness;

pub use crate::units::{
    duration::{Delay, Interval, MAX_DURATION},
    ratio::{Fraction, Psi},
    swappiness::Swappiness,
};
