pub mod service {
    use metrics::{
        RegistersMetrics,
        prometheus::{IntGauge, Opts, core::Collector},
    };

    use crate::{Result, Runner, fees::Api};

    #[derive(Debug, Clone)]
    struct FeeMetrics {
        current: IntGauge,
    }

    impl Default for FeeMetrics {
        fn default() -> Self {
            let current = IntGauge::with_opts(Opts::new(
                "current_blob_tx_fee",
                "The current fee for a transaction with 6 blobs",
            ))
            .expect("metric config to be correct");

            Self { current }
        }
    }

    impl<P> RegistersMetrics for FeeMetricsTracker<P> {
        fn metrics(&self) -> Vec<Box<dyn Collector>> {
            vec![Box::new(self.metrics.current.clone())]
        }
    }

    #[derive(Clone)]
    pub struct FeeMetricsTracker<P> {
        _fee_provider: P,
        metrics: FeeMetrics,
    }

    impl<P> FeeMetricsTracker<P> {
        pub fn new(fee_provider: P) -> Self {
            Self {
                _fee_provider: fee_provider,
                metrics: FeeMetrics::default(),
            }
        }
    }

    impl<P: Api> FeeMetricsTracker<P> {
        pub async fn update_metrics(&self) -> Result<()> {
            Ok(())
        }
    }

    impl<P> Runner for FeeMetricsTracker<P>
    where
        P: Api + Send + Sync,
    {
        async fn run(&mut self) -> Result<()> {
            self.update_metrics().await?;
            Ok(())
        }
    }
}
