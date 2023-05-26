use prometheus::{core::Collector, Registry};

pub trait RegistersMetrics {
    fn register_metrics(&self, registry: &Registry) {
        self.metrics().into_iter().for_each(|metric| {
            registry
                .register(metric)
                .expect("app to have correctly named metrics");
        });
    }

    fn metrics(&self) -> Vec<Box<dyn Collector>>;
}
