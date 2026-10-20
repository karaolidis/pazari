//! The bounded scalars pazari measures itself in.

pub mod duration;
pub mod psi;
pub mod swappiness;

pub use crate::units::{
    duration::{Delay, Interval},
    psi::Psi,
    swappiness::Swappiness,
};
