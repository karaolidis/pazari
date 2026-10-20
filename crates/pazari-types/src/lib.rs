//! Strict domain types for pazari.

pub mod backoff;
pub mod bounded;
pub mod deadline;
pub mod error;
pub mod kernel;
pub mod scale;
pub mod time;
pub mod units;

pub use backoff::Backoff;
pub use bounded::Bounded;
pub use deadline::Deadline;
pub use error::{KernelFileError, RangeError};
pub use kernel::{MemInfo, Pressure};
pub use scale::Scale;
pub use time::{Clock, Timestamp};
pub use units::{Delay, Interval, Psi, Swappiness};
