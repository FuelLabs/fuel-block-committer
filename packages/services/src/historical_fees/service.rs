use std::{num::NonZeroU64, ops::RangeInclusive};

use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};

use super::{
    fee_analytics::{self, FeeAnalytics},
    port::l1::{Api, Fees},
};
use crate::{Result, Runner};

#[derive(Debug, Clone)]
struct Metrics {
    current_blob_tx_fee: IntGauge,
    short_term_blob_tx_fee: IntGauge,
    long_term_blob_tx_fee: IntGauge,
}

impl Default for Metrics {
    fn default() -> Self {
        let current_blob_tx_fee = IntGauge::with_opts(Opts::new(
            "current_blob_tx_fee",
            "The current fee for a transaction with 6 blobs",
        ))
        .expect("metric config to be correct");

        let short_term_blob_tx_fee = IntGauge::with_opts(Opts::new(
            "short_term_blob_tx_fee",
            "The short term fee for a transaction with 6 blobs",
        ))
        .expect("metric config to be correct");

        let long_term_blob_tx_fee = IntGauge::with_opts(Opts::new(
            "long_term_blob_tx_fee",
            "The long term fee for a transaction with 6 blobs",
        ))
        .expect("metric config to be correct");

        Self {
            current_blob_tx_fee,
            short_term_blob_tx_fee,
            long_term_blob_tx_fee,
        }
    }
}

impl<P> RegistersMetrics for FeeMetricsUpdater<P> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.current_blob_tx_fee.clone()),
            Box::new(self.metrics.short_term_blob_tx_fee.clone()),
            Box::new(self.metrics.long_term_blob_tx_fee.clone()),
        ]
    }
}

#[derive(Clone)]
pub struct FeeMetricsUpdater<P> {
    fee_analytics: FeeAnalytics<P>,
    metrics: Metrics,
    metrics_sma: SmaPeriods,
}

#[derive(Debug, Clone, Copy)]
pub struct SmaPeriods {
    pub short: NonZeroU64,
    pub long: NonZeroU64,
}
impl<P: Api> FeeMetricsUpdater<P> {
    fn last_n_blocks(current_block: u64, n: NonZeroU64) -> RangeInclusive<u64> {
        current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
    }

    pub async fn update_metrics(&self) -> Result<()> {
        let latest_fees = self.fee_analytics.latest_fees().await?;
        let short_term_sma = self
            .fee_analytics
            .calculate_sma(Self::last_n_blocks(
                latest_fees.height,
                self.metrics_sma.short,
            ))
            .await?;

        let long_term_sma = self
            .fee_analytics
            .calculate_sma(Self::last_n_blocks(
                latest_fees.height,
                self.metrics_sma.long,
            ))
            .await?;

        let calc_fee = |fees: &Fees| {
            i64::try_from(fee_analytics::calculate_blob_tx_fee(6, fees)).unwrap_or(i64::MAX)
        };

        self.metrics
            .current_blob_tx_fee
            .set(calc_fee(&latest_fees.fees));
        self.metrics
            .short_term_blob_tx_fee
            .set(calc_fee(&short_term_sma));
        self.metrics
            .long_term_blob_tx_fee
            .set(calc_fee(&long_term_sma));

        Ok(())
    }
}

impl<P> FeeMetricsUpdater<P> {
    pub fn new(fee_analytics: FeeAnalytics<P>, metrics_sma: SmaPeriods) -> Self {
        Self {
            fee_analytics,
            metrics_sma,
            metrics: Metrics::default(),
        }
    }
}

impl<P> Runner for FeeMetricsUpdater<P>
where
    P: crate::historical_fees::port::l1::Api + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        self.update_metrics().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    

    
    
}
