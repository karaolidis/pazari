//! Prometheus metrics shared by the pazari binaries.

pub mod error;
pub mod label;
pub mod server;

use std::{fmt, sync::atomic::AtomicU64};

use prometheus_client::encoding::text::encode;

pub use prometheus_client::{
    metrics::{counter::Counter, family::Family, gauge::Gauge},
    registry::{Registry, Unit},
};

pub use crate::{
    error::MetricsError,
    label::{GuestLabel, KindLabel, ModeLabel, ReasonLabel},
    server::Exposition,
};

pub type Fraction = Gauge<f64, AtomicU64>;

pub trait Saturating {
    fn set_saturating(&self, value: u64);
}

impl Saturating for Gauge {
    fn set_saturating(&self, value: u64) {
        self.set(i64::try_from(value).unwrap_or(i64::MAX));
    }
}

pub trait Encoded {
    fn encoded(&self) -> Result<String, fmt::Error>;
}

impl Encoded for Registry {
    fn encoded(&self) -> Result<String, fmt::Error> {
        let mut body = String::new();
        encode(&mut body, self)?;
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_over_what_a_gauge_holds_stops_at_the_top_of_it() {
        let gauge = Gauge::default();

        gauge.set_saturating(0);
        assert_eq!(gauge.get(), 0);

        gauge.set_saturating(4096);
        assert_eq!(gauge.get(), 4096);

        gauge.set_saturating(u64::MAX);
        assert_eq!(gauge.get(), i64::MAX);
    }
}
