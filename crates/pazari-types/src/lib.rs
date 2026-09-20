//! Strict domain types for pazari.

pub mod backoff;
pub mod bounded;
pub mod duration;
pub mod error;
pub mod meminfo;
pub mod pressure;
pub mod ratio;
pub mod swappiness;
pub mod time;

pub use backoff::Backoff;
pub use bounded::Bounded;
pub use duration::{Delay, Interval, MAX_DURATION};
pub use error::{KernelFileError, RangeError};
pub use meminfo::MemInfo;
pub use pressure::Pressure;
pub use ratio::{Fraction, Psi};
pub use swappiness::Swappiness;
pub use time::{Clock, Timestamp};
