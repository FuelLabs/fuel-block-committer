#![deny(unused_crate_dependencies)]

mod connection_health_tracker;
pub use connection_health_tracker::*;

pub type HealthChecker = Box<dyn HealthCheck>;
pub trait HealthCheck: Send + Sync {
    fn healthy(&self) -> bool;
}

pub use prometheus;

pub trait RegistersMetrics {
    fn register_metrics(&self, registry: &crate::prometheus::Registry) {
        self.metrics().into_iter().for_each(|metric| {
            registry
                .register(metric)
                .expect("app to have correctly named metrics");
        });
    }

    fn metrics(&self) -> Vec<Box<dyn crate::prometheus::core::Collector>>;
}

pub fn custom_exponential_buckets(start: f64, end: f64, steps: usize) -> Vec<f64> {
    let factor = (end / start).powf(1.0 / (steps - 1) as f64);
    let mut buckets = Vec::with_capacity(steps);

    let mut value = start;
    for _ in 0..(steps - 1) {
        buckets.push(value.ceil());
        value *= factor;
    }

    buckets.push(end.ceil());

    buckets
}
