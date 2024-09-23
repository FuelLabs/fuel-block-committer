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
