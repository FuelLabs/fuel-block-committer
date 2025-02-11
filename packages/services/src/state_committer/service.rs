use std::{num::NonZeroUsize, time::Duration};

use itertools::Itertools;
use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use tracing::info;

use super::{fee_algo::SmaFeeAlgo, AlgoConfig};
use crate::{
    state_committer::port::da_layer::Priority,
    types::{
        storage::BundleFragment, CollectNonEmpty, DateTime, EthereumDASubmission, NonEmpty, Utc,
    },
    Result, Runner,
};

// src/config.rs
#[derive(Debug, Clone)]
pub struct Config {
    /// The lookback window in blocks to determine the starting height.
    pub lookback_window: u32,
    pub fragment_accumulation_timeout: Duration,
    pub fragments_to_accumulate: NonZeroUsize,
    pub gas_bump_timeout: Duration,
    pub fee_algo: AlgoConfig,
}

#[cfg(feature = "test-helpers")]
impl Default for Config {
    fn default() -> Self {
        Self {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(0),
            fragments_to_accumulate: 1.try_into().unwrap(),
            gas_bump_timeout: Duration::from_secs(300),
            fee_algo: AlgoConfig::default(),
        }
    }
}

struct Metrics {
    current_height_to_commit: IntGauge,
}

impl Default for Metrics {
    fn default() -> Self {
        let current_height_to_commit = IntGauge::with_opts(Opts::new(
            "current_height_to_commit",
            "The starting l2 height of the bundle we're committing/will commit next",
        ))
        .expect("metric config to be correct");

        Self {
            current_height_to_commit,
        }
    }
}

impl<DALayer, FuelApi, Db, Clock, D> RegistersMetrics
    for StateCommitter<DALayer, FuelApi, Db, Clock, D>
{
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![Box::new(self.metrics.current_height_to_commit.clone())]
    }
}

/// The `StateCommitter` is responsible for committing state fragments to L1.
pub struct StateCommitter<DALayer, FuelApi, Db, Clock, FeeProvider> {
    da_layer: DALayer,
    fuel_api: FuelApi,
    storage: Db,
    config: Config,
    clock: Clock,
    startup_time: DateTime<Utc>,
    metrics: Metrics,
    fee_algo: SmaFeeAlgo<FeeProvider>,
}

impl<DALayer, FuelApi, Db, Clock, FeeProvider>
    StateCommitter<DALayer, FuelApi, Db, Clock, FeeProvider>
where
    Clock: crate::state_committer::port::Clock,
{
    /// Creates a new `StateCommitter`.
    pub fn new(
        da_layer: DALayer,
        fuel_api: FuelApi,
        storage: Db,
        config: Config,
        clock: Clock,
        fee_provider: FeeProvider,
    ) -> Self {
        let startup_time = clock.now();

        Self {
            fee_algo: SmaFeeAlgo::new(fee_provider, config.fee_algo),
            da_layer,
            fuel_api,
            storage,
            config,
            clock,
            startup_time,
            metrics: Metrics::default(),
        }
    }
}

impl<DALayer, FuelApi, Db, Clock, FeeProvider>
    StateCommitter<DALayer, FuelApi, Db, Clock, FeeProvider>
where
    DALayer: crate::state_committer::port::da_layer::Api + Send + Sync,
    FuelApi: crate::state_committer::port::fuel::Api,
    Db: crate::state_committer::port::Storage,
    Clock: crate::state_committer::port::Clock,
    FeeProvider: crate::fees::Api + Sync,
{
    async fn get_reference_time(&self) -> Result<DateTime<Utc>> {
        Ok(self
            .storage
            .last_time_a_fragment_was_finalized()
            .await?
            .unwrap_or(self.startup_time))
    }

    async fn is_timeout_expired(&self) -> Result<bool> {
        let reference_time = self.get_reference_time().await?;
        let elapsed = self.clock.now() - reference_time;
        let std_elapsed = elapsed
            .to_std()
            .map_err(|e| crate::Error::Other(format!("Failed to convert time: {e}")))?;
        Ok(std_elapsed >= self.config.fragment_accumulation_timeout)
    }

    async fn l2_blocks_behind(&self, fragments: &NonEmpty<BundleFragment>) -> Result<u32> {
        let l2_height = self.fuel_api.latest_height().await?;

        let oldest_l2_block = Self::oldest_l2_block_in_fragments(fragments);
        self.update_oldest_block_metric(oldest_l2_block);

        Ok(l2_height.saturating_sub(oldest_l2_block))
    }

    async fn fees_acceptable(&self, fragments: &NonEmpty<BundleFragment>) -> Result<bool> {
        let l1_height = self.da_layer.current_height().await?;
        let num_l2_blocks_behind = self.l2_blocks_behind(fragments).await?;
        let num_blobs =
            u32::try_from(fragments.len()).expect("not to send more than u32::MAX blobs");

        self.fee_algo
            .fees_acceptable(num_blobs, num_l2_blocks_behind, l1_height)
            .await
    }

    fn oldest_l2_block_in_fragments(fragments: &NonEmpty<BundleFragment>) -> u32 {
        fragments
            .minimum_by_key(|b| b.oldest_block_in_bundle)
            .oldest_block_in_bundle
    }

    async fn determine_priority(&self, fragments: &NonEmpty<BundleFragment>) -> Result<Priority> {
        let blocks_behind = self.l2_blocks_behind(fragments).await? as f64;

        let max_l2_behind = self
            .config
            .fee_algo
            .fee_thresholds
            .max_l2_blocks_behind
            .get() as f64;

        let percentage = blocks_behind / max_l2_behind * 100.;

        let capped_at_100 = percentage.min(100.);

        Priority::new(capped_at_100)
    }

    async fn submit_fragments(
        &self,
        fragments: NonEmpty<BundleFragment>,
        previous_tx: Option<EthereumDASubmission>,
    ) -> Result<()> {
        info!("about to send at most {} fragments", fragments.len());

        let data = fragments.clone().map(|f| f.fragment);

        let priority = self.determine_priority(&fragments).await?;
        match self
            .da_layer
            .submit_state_fragments(data.clone(), previous_tx, priority)
            .await
        {
            Ok((submitted_tx, submitted_fragments)) => {
                let fragment_ids = fragments
                    .iter()
                    .map(|f| f.id)
                    .take(submitted_fragments.num_fragments.get())
                    .collect_nonempty()
                    .expect("non-empty vec");

                let ids = fragment_ids
                    .iter()
                    .map(|id| id.as_u32().to_string())
                    .join(", ");

                let tx_hash = submitted_tx.hash;
                self.storage
                    .record_pending_tx(submitted_tx, fragment_ids, self.clock.now())
                    .await?;

                tracing::info!("Submitted fragments {ids} with tx {}", hex::encode(tx_hash));
                Ok(())
            }
            Err(e) => {
                let ids = fragments
                    .iter()
                    .map(|f| f.id.as_u32().to_string())
                    .join(", ");

                tracing::error!("Failed to submit fragments {ids}: {e}");

                Err(e)
            }
        }
    }

    async fn latest_pending_transaction(&self) -> Result<Option<EthereumDASubmission>> {
        let tx = self.storage.get_latest_pending_txs().await?;
        Ok(tx)
    }

    async fn next_fragments_to_submit(&self) -> Result<Option<NonEmpty<BundleFragment>>> {
        let latest_height = self.fuel_api.latest_height().await?;
        let starting_height = latest_height.saturating_sub(self.config.lookback_window);

        // although we shouldn't know at this layer how many fragments the L1 can accept, we ignore
        // this for now and put the eth value of max blobs per block (6).
        let existing_fragments = self
            .storage
            .oldest_nonfinalized_fragments(starting_height, 6)
            .await?;

        let fragments = NonEmpty::collect(existing_fragments);

        if let Some(fragments) = fragments.as_ref() {
            // Tracking the metric here as well to get updates more often -- because
            // submit_fragments might not be called
            self.update_oldest_block_metric(Self::oldest_l2_block_in_fragments(fragments));
        }

        Ok(fragments)
    }

    fn update_oldest_block_metric(&self, oldest_height: u32) {
        self.metrics
            .current_height_to_commit
            .set(oldest_height.into());
    }

    async fn should_submit_fragments(&self, fragments: &NonEmpty<BundleFragment>) -> Result<bool> {
        let fragment_count = fragments.len_nonzero();

        let expired = || async {
            let expired = self.is_timeout_expired().await?;
            if expired {
                info!(
                    "fragment accumulation timeout expired, available {}/{} fragments",
                    fragment_count, self.config.fragments_to_accumulate
                );
            }
            Result::Ok(expired)
        };

        let enough_fragments = || {
            let enough_fragments = fragment_count >= self.config.fragments_to_accumulate;
            if !enough_fragments {
                info!(
                    "not enough fragments {}/{}",
                    fragment_count, self.config.fragments_to_accumulate
                );
            };
            enough_fragments
        };

        // wrapped in closures so that we short-circuit *and* reduce redundant logs
        Ok(enough_fragments() || expired().await?)
    }

    fn elapsed_since_tx_submitted(&self, tx: &EthereumDASubmission) -> Result<Duration> {
        let created_at = tx.created_at.expect("tx to have timestamp");

        self.clock.elapsed(created_at)
    }

    async fn fragments_submitted_by_tx(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<NonEmpty<BundleFragment>> {
        let fragments = self.storage.fragments_submitted_by_tx(tx_hash).await?;

        NonEmpty::collect(fragments).ok_or_else(|| {
            crate::Error::Other(format!(
                "no fragments found for previously submitted tx {}",
                hex::encode(tx_hash)
            ))
        })
    }

    async fn submit_fragments_if_ready(&self, fragments: NonEmpty<BundleFragment>) -> Result<()> {
        if self.should_submit_fragments(&fragments).await?
            && self.fees_acceptable(&fragments).await?
        {
            self.submit_fragments(fragments, None).await?;
        }

        Ok(())
    }

    async fn fetch_stalled_submission(&self) -> Result<Option<EthereumDASubmission>> {
        if let Some(submission) = self.latest_pending_transaction().await? {
            let elapsed = self.elapsed_since_tx_submitted(&submission)?;

            if elapsed >= self.config.gas_bump_timeout {
                info!(
                    "tx {} needs to be replaced because it was pending for {}s",
                    hex::encode(submission.hash),
                    elapsed.as_secs()
                );

                return Ok(Some(submission));
            }
        };

        Ok(None)
    }

    async fn resubmit_fragments(&self, previos_submission: EthereumDASubmission) -> Result<()> {
        let fragments = self
            .fragments_submitted_by_tx(previos_submission.hash)
            .await?;

        if self.fees_acceptable(&fragments).await? {
            self.submit_fragments(fragments, Some(previos_submission))
                .await?;
        }

        Ok(())
    }

    async fn update_current_height_to_commit_metric(&self) -> Result<()> {
        let current_height_to_commit =
            if let Some(height) = self.storage.latest_bundled_height().await? {
                height.saturating_add(1)
            } else {
                self.fuel_api
                    .latest_height()
                    .await?
                    .saturating_sub(self.config.lookback_window)
            };

        self.metrics
            .current_height_to_commit
            .set(current_height_to_commit.into());

        Ok(())
    }
}

impl<DALayer, FuelApi, Db, Clock, FeeProvider> Runner
    for StateCommitter<DALayer, FuelApi, Db, Clock, FeeProvider>
where
    DALayer: crate::state_committer::port::da_layer::Api + Send + Sync,
    FuelApi: crate::state_committer::port::fuel::Api + Send + Sync,
    Db: crate::state_committer::port::Storage + Clone + Send + Sync,
    Clock: crate::state_committer::port::Clock + Send + Sync,
    FeeProvider: crate::fees::Api + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        if let Some(submission) = self.fetch_stalled_submission().await? {
            // if we have a stalled submission, we need to resubmit it
            self.resubmit_fragments(submission).await?;
        } else if let Some(fragments) = self.next_fragments_to_submit().await? {
            // else if we have fragments to submit, we should do so
            self.submit_fragments_if_ready(fragments).await?;
        } else {
            // else we're up to date with submissions and new blocks haven't been bundled yet
            self.update_current_height_to_commit_metric().await?;
        };

        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        fees::testing::PreconfiguredFeeApi,
        state_committer::{FeeThresholds, SmaPeriods},
    };

    #[tokio::test]
    async fn test_send_when_too_far_behind_and_fee_provider_fails() {
        // given
        let config = AlgoConfig {
            sma_periods: SmaPeriods {
                short: 2.try_into().unwrap(),
                long: 6.try_into().unwrap(),
            },
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 10.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            },
        };

        // having no fees will make the validation in fee analytics fail
        let api = PreconfiguredFeeApi::new(vec![]);
        let sut = SmaFeeAlgo::new(api, config);

        // when
        let should_send = sut
            .fees_acceptable(1, 20, 100)
            .await
            .expect("Should send despite fee provider failure");

        // then
        assert!(
            should_send,
            "Should send because too far behind, regardless of fee provider status"
        );
    }
}
