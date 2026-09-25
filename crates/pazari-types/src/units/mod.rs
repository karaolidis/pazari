//! The bounded scalars pazari measures itself in.

pub mod duration;
pub mod psi;
pub mod swappiness;

pub use crate::units::{
    duration::{Delay, Interval, MAX_DURATION},
    psi::Psi,
    swappiness::Swappiness,
};
