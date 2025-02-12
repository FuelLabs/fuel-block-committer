use std::time::Duration;

use itertools::Itertools;
use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use tracing::info;

use crate::{
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
}

#[cfg(feature = "test-helpers")]
impl Default for Config {
    fn default() -> Self {
        Self {
            lookback_window: 1000,
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

impl<DALayer, FuelApi, Db, Clock> RegistersMetrics for StateCommitter<DALayer, FuelApi, Db, Clock> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![Box::new(self.metrics.current_height_to_commit.clone())]
    }
}

/// The `StateCommitter` is responsible for committing state fragments to L1.
pub struct StateCommitter<DALayer, FuelApi, Db, Clock> {
    da_layer: DALayer,
    fuel_api: FuelApi,
    storage: Db,
    config: Config,
    clock: Clock,
    startup_time: DateTime<Utc>,
    metrics: Metrics,
}

impl<DALayer, FuelApi, Db, Clock> StateCommitter<DALayer, FuelApi, Db, Clock>
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
    ) -> Self {
        let startup_time = clock.now();

        Self {
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

impl<DALayer, FuelApi, Db, Clock> StateCommitter<DALayer, FuelApi, Db, Clock>
where
    DALayer: crate::state_committer::port::eigen_da::Api + Send + Sync,
    FuelApi: crate::state_committer::port::fuel::Api,
    Db: crate::state_committer::port::Storage,
    Clock: crate::state_committer::port::Clock,
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

    fn oldest_l2_block_in_fragments(fragments: &NonEmpty<BundleFragment>) -> u32 {
        fragments
            .minimum_by_key(|b| b.oldest_block_in_bundle)
            .oldest_block_in_bundle
    }

    async fn submit_fragments(
        &self,
        fragments: BundleFragment,
        previous_tx: Option<EthereumDASubmission>,
    ) -> Result<()> {
        info!("about to send at most {} fragments", fragments.len());

        let data = fragments.clone().map(|f| f.fragment);

        match self.da_layer.submit_state_fragment(data.clone()).await {
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
        // this for now and put the eigen value of max blobs per request (1).
        let existing_fragments = self
            .storage
            .oldest_nonfinalized_fragments(starting_height, 1)
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
        // TODO check if data accumulation timeout has expired

        Ok(true)
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
        if self.should_submit_fragments(&fragments).await? {
            self.submit_fragments(fragments.head, None).await?;
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

impl<DALayer, FuelApi, Db, Clock> Runner for StateCommitter<DALayer, FuelApi, Db, Clock>
where
    DALayer: crate::state_committer::port::eigen_da::Api + Send + Sync,
    FuelApi: crate::state_committer::port::fuel::Api + Send + Sync,
    Db: crate::state_committer::port::Storage + Clone + Send + Sync,
    Clock: crate::state_committer::port::Clock + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        if let Some(fragments) = self.next_fragments_to_submit().await? {
            // else if we have fragments to submit, we should do so
            self.submit_fragments_if_ready(fragments).await?;
        } else {
            // else we're up to date with submissions and new blocks haven't been bundled yet
            self.update_current_height_to_commit_metric().await?;
        };

        Ok(())
    }
}
