use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use nonempty::nonempty;
use tracing::info;

use crate::{types::storage::BundleFragment, Result, Runner};

use super::commit_helpers::{next_fragments_to_submit, update_current_height_to_commit_metric};

// src/config.rs
#[derive(Debug, Clone)]
pub struct Config {
    // The throughput of the eigen API in MiB/s.
    pub api_throughput: u32,
    /// The lookback window in blocks to determine the starting height.
    pub lookback_window: u32,
}

#[cfg(feature = "test-helpers")]
impl Default for Config {
    fn default() -> Self {
        Self {
            api_throughput: 16,
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
        Self {
            da_layer,
            fuel_api,
            storage,
            config,
            clock,
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
    async fn submit_fragment(&self, fragment: BundleFragment) -> Result<()> {
        info!(
            "about to send a fragment with size: {}",
            fragment.fragment.data.len()
        );

        let fragment_id = fragment.id;
        match self.da_layer.submit_state_fragment(fragment.fragment).await {
            Ok(submitted_tx) => {
                let tx_hash = submitted_tx.hash;
                self.storage
                    .record_da_submission(submitted_tx, nonempty![fragment.id], self.clock.now())
                    .await?;

                tracing::info!(
                    "Submitted fragment {fragment_id} with tx {}",
                    hex::encode(tx_hash)
                );
                Ok(())
            }
            Err(e) => {
                tracing::error!("Failed to submit fragment {fragment_id}: {e}");
                Err(e)
            }
        }
    }

    fn update_oldest_block_metric(&self, oldest_height: u32) {
        self.metrics
            .current_height_to_commit
            .set(oldest_height.into());
    }

    async fn next_fragment_to_submit(&self) -> Result<Option<BundleFragment>> {
        // Eigen accepts one fragment (blob) per submission.
        let fragment = next_fragments_to_submit(
            &self.fuel_api,
            &self.storage,
            self.config.lookback_window,
            1,
        )
        .await?
        .map(|frags| frags.head);

        if let Some(ref frag) = fragment {
            self.update_oldest_block_metric(frag.oldest_block_in_bundle);
        }

        Ok(fragment)
    }

    async fn should_submit(&self, fragment: &BundleFragment) -> Result<bool> {
        // TODO check if eigen API is ready to accept more data
        let _size = fragment.fragment.data.len();

        Ok(true)
    }

    async fn submit_fragment_if_ready(&self, fragment: BundleFragment) -> Result<()> {
        if self.should_submit(&fragment).await? {
            self.submit_fragment(fragment).await?;
        }

        Ok(())
    }

    async fn update_current_height_to_commit_metric(&self) -> Result<()> {
        update_current_height_to_commit_metric(
            &self.fuel_api,
            &self.storage,
            self.config.lookback_window,
            &self.metrics.current_height_to_commit,
        )
        .await
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
        if let Some(fragment) = self.next_fragment_to_submit().await? {
            self.submit_fragment_if_ready(fragment).await?;
        } else {
            self.update_current_height_to_commit_metric().await?;
        };

        Ok(())
    }
}
