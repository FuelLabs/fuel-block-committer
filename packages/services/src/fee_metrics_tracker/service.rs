use std::{num::NonZeroU64, ops::RangeInclusive};

use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};

use super::port::l1::{Api, BlockFees, Fees};
use crate::{Error, Result, Runner};

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

impl<P> RegistersMetrics for FeeMetricsTracker<P> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.current_blob_tx_fee.clone()),
            Box::new(self.metrics.short_term_blob_tx_fee.clone()),
            Box::new(self.metrics.long_term_blob_tx_fee.clone()),
        ]
    }
}

#[derive(Clone)]
pub struct FeeMetricsTracker<P> {
    fee_provider: P,
    sma_periods: SmaPeriods,
    metrics: Metrics,
}

#[derive(Debug, Clone, Copy)]
pub struct SmaPeriods {
    pub short: NonZeroU64,
    pub long: NonZeroU64,
}

pub fn calculate_blob_tx_fee(num_blobs: u32, fees: &Fees) -> u128 {
    const DATA_GAS_PER_BLOB: u128 = 131_072u128;
    const INTRINSIC_GAS: u128 = 21_000u128;

    let base_fee = INTRINSIC_GAS.saturating_mul(fees.base_fee_per_gas);
    let blob_fee = fees
        .base_fee_per_blob_gas
        .saturating_mul(u128::from(num_blobs))
        .saturating_mul(DATA_GAS_PER_BLOB);
    let reward_fee = fees.reward.saturating_mul(INTRINSIC_GAS);

    base_fee.saturating_add(blob_fee).saturating_add(reward_fee)
}

impl<P: Api> FeeMetricsTracker<P> {
    pub async fn calculate_sma(&self, block_range: RangeInclusive<u64>) -> crate::Result<Fees> {
        let fees = self.fee_provider.fees(block_range.clone()).await?;

        let received_height_range = fees.height_range();
        if received_height_range != block_range {
            return Err(Error::from(format!(
                "fees received from the adapter({received_height_range:?}) don't cover the requested range ({block_range:?})"
            )));
        }

        Ok(fees.mean())
    }

    pub async fn latest_fees(&self) -> crate::Result<BlockFees> {
        let height = self.fee_provider.current_height().await?;

        let fee = self
            .fee_provider
            .fees(height..=height)
            .await?
            .into_iter()
            .next()
            .expect("sequential fees guaranteed not empty");

        Ok(fee)
    }
}

fn last_n_blocks(current_block: u64, n: NonZeroU64) -> RangeInclusive<u64> {
    current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
}

impl<P> FeeMetricsTracker<P> {
    pub fn new(fee_provider: P, sma_periods: SmaPeriods) -> Self {
        Self {
            fee_provider,
            sma_periods,
            metrics: Metrics::default(),
        }
    }
}

impl<P: Api> FeeMetricsTracker<P> {
    pub async fn update_metrics(&self) -> Result<()> {
        let metrics_sma = self.sma_periods;
        let current_block = self.fee_provider.current_height().await?;
        let latest_fees = self
            .fee_provider
            .fees(last_n_blocks(
                current_block,
                1.try_into().expect("not zero"),
            ))
            .await?
            .mean();
        let short_term_sma = self
            .fee_provider
            .fees(last_n_blocks(current_block, metrics_sma.short))
            .await?
            .mean();

        let long_term_sma = self
            .fee_provider
            .fees(last_n_blocks(current_block, metrics_sma.long))
            .await?
            .mean();

        let calc_fee =
            |fees: &Fees| i64::try_from(calculate_blob_tx_fee(6, fees)).unwrap_or(i64::MAX);

        self.metrics.current_blob_tx_fee.set(calc_fee(&latest_fees));
        self.metrics
            .short_term_blob_tx_fee
            .set(calc_fee(&short_term_sma));
        self.metrics
            .long_term_blob_tx_fee
            .set(calc_fee(&long_term_sma));

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
