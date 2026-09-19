//! Strict domain types for pazari.

pub mod bounded;
pub mod duration;
pub mod error;
pub mod meminfo;
pub mod ratio;
pub mod swappiness;
pub mod time;

pub use bounded::Bounded;
pub use duration::{Delay, Interval, MAX_DURATION};
pub use error::RangeError;
pub use meminfo::{MemInfo, MemInfoError};
pub use ratio::{Fraction, Psi};
pub use swappiness::Swappiness;
pub use time::{Clock, Timestamp};
