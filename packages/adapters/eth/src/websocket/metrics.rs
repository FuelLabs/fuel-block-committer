
use alloy::eips::eip4844::BYTES_PER_BLOB;
use metrics::prometheus::{self, histogram_opts};

#[derive(Clone)]
pub struct Metrics {
    pub(crate) blobs_per_tx: prometheus::Histogram,
    pub(crate) blob_unused_bytes: prometheus::Histogram,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            blobs_per_tx: prometheus::Histogram::with_opts(histogram_opts!(
                "blob_per_tx",
                "Number of blobs per blob transaction",
                vec![1.0f64, 2., 3., 4., 5., 6.]
            ))
            .expect("to be correctly configured"),

            blob_unused_bytes: prometheus::Histogram::with_opts(histogram_opts!(
                "blob_unused_bytes",
                "unused bytes per blob",
                metrics::custom_exponential_buckets(1000f64, BYTES_PER_BLOB as f64, 20)
            ))
            .expect("to be correctly configured"),
        }
    }
}
