use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, Ordering},
};

use prometheus_client::{
    encoding::{EncodeGaugeValue, EncodeMetric, MetricEncoder},
    metrics::{
        MetricType, TypedMetric,
        gauge::{Atomic, Gauge},
    },
};

use crate::{Saturating, saturated};

/// A gauge with no sample until it is first set.
#[derive(Debug)]
pub struct Sampled<N = i64, A = AtomicI64> {
    gauge: Gauge<N, A>,
    sampled: Arc<AtomicBool>,
}

impl<N, A> Clone for Sampled<N, A> {
    fn clone(&self) -> Self {
        Self {
            gauge: self.gauge.clone(),
            sampled: Arc::clone(&self.sampled),
        }
    }
}

impl<N, A: Atomic<N> + Default> Default for Sampled<N, A> {
    fn default() -> Self {
        Self {
            gauge: Gauge::default(),
            sampled: Arc::default(),
        }
    }
}

impl<N, A: Atomic<N>> Sampled<N, A> {
    pub fn set(&self, value: N) {
        self.gauge.set(value);
        self.sampled.store(true, Ordering::Release);
    }

    fn sampled(&self) -> bool {
        self.sampled.load(Ordering::Acquire)
    }
}

impl Saturating for Sampled {
    fn set_saturating(&self, value: u64) {
        self.set(saturated(value));
    }
}

impl<N, A> TypedMetric for Sampled<N, A> {
    const TYPE: MetricType = MetricType::Gauge;
}

impl<N: EncodeGaugeValue, A: Atomic<N>> EncodeMetric for Sampled<N, A> {
    fn encode(&self, mut encoder: MetricEncoder) -> Result<(), std::fmt::Error> {
        if !self.sampled() {
            return Ok(());
        }

        encoder.encode_gauge(&self.gauge.get())
    }

    fn metric_type(&self) -> MetricType {
        Self::TYPE
    }

    fn is_empty(&self) -> bool {
        !self.sampled()
    }
}

#[cfg(test)]
mod tests {
    use prometheus_client::registry::Registry;

    use super::*;
    use crate::Encoded;

    fn registry(gauge: &Sampled) -> Registry {
        let mut registry = Registry::default();

        registry.register("reading", "A reading", gauge.clone());

        registry
    }

    #[test]
    fn a_gauge_never_set_is_left_out() {
        let gauge = Sampled::default();

        let body = registry(&gauge).encoded().expect("the registry encodes");

        assert!(!body.contains("reading"), "unexpected body: {body}");
    }

    #[test]
    fn a_gauge_once_set_carries_its_value() {
        let gauge = Sampled::default();

        gauge.set_saturating(4096);

        let body = registry(&gauge).encoded().expect("the registry encodes");

        assert!(body.contains("reading 4096\n"), "unexpected body: {body}");
    }
}
