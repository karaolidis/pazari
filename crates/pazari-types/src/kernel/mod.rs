//! Parsers for the kernel files pazari reads.

pub mod meminfo;
pub mod pressure;

pub use crate::kernel::{
    meminfo::{KB, MemInfo},
    pressure::Pressure,
};
