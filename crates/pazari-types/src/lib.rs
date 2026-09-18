//! Strict domain types for pazari.

pub mod duration;
pub mod error;
pub mod ratio;
pub mod swappiness;

pub use duration::{Delay, Interval, MAX_DURATION};
pub use error::RangeError;
pub use ratio::{Fraction, Psi};
pub use swappiness::Swappiness;
