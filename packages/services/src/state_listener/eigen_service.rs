
use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use tracing::info;

use crate::{
    types::{EigenDASubmission, TransactionState},
    Runner,
};

pub struct StateListener<EigenDA, Db, Clock> {
    eigenda_adapter: EigenDA,
    storage: Db,
    metrics: Metrics,
    clock: Clock,
}

impl<EigenDA, Db, Clock> StateListener<EigenDA, Db, Clock> {
    pub fn new(
        l1_adapter: EigenDA,
        storage: Db,
        clock: Clock,
        last_finalization_time_metric: IntGauge,
    ) -> Self {
        Self {
            eigenda_adapter: l1_adapter,
            storage,
            metrics: Metrics::new(last_finalization_time_metric),
            clock,
        }
    }
}

impl<EigenDA, Db, Clock> StateListener<EigenDA, Db, Clock>
where
    EigenDA: crate::state_listener::port::eigen_da::Api,
    Db: crate::state_listener::port::Storage,
    Clock: crate::state_listener::port::Clock,
{
    async fn check_non_finalized(
        &self,
        non_finalized: Vec<EigenDASubmission>,
    ) -> crate::Result<()> {
        let mut changes = Vec::with_capacity(non_finalized.len());

        for submission in non_finalized {
            let status = self.eigenda_adapter.get_blob_status(vec![]).await?;

            if status.is_processing() {
                // TODO: maybe log how long it was in processing
                continue;
            } else if status.is_confirmed() { 
                changes.push((submission.hash, TransactionState::IncludedInBlock));
                // TODO confirmed handling
            } else if status.is_finalized() {
                // log finalization
                changes.push((submission.hash, TransactionState::Finalized(self.clock.now())));
            } else {
                changes.push((submission.hash, TransactionState::Failed));
                // TODO log failure
            }
        }

        // TODO: cost calculation and maybe don't reuse the tx update method
                self.storage.update_tx_states_and_costs(changes, vec![], vec![]).await?;

        Ok(())
    }
}

impl<EigenDA, Db, Clock> Runner for StateListener<EigenDA, Db, Clock>
where
EigenDA: crate::state_listener::port::eigen_da::Api + Send + Sync,
    Db: crate::state_listener::port::Storage,
    Clock: crate::state_listener::port::Clock + Send + Sync,
{
    async fn run(&mut self) -> crate::Result<()> {
        let non_finalized = self.storage.get_non_finalized_submissions().await?;

        if non_finalized.is_empty() {
            return Ok(());
        }

        self.check_non_finalized(non_finalized).await?;

        Ok(())
    }
}

#[derive(Clone)]
struct Metrics {
    last_eth_block_w_blob: IntGauge,
    last_finalization_time: IntGauge,
    last_finalization_interval: IntGauge,
}

impl<L1, Db, Clock> RegistersMetrics for StateListener<L1, Db, Clock> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.last_eth_block_w_blob.clone()),
            Box::new(self.metrics.last_finalization_time.clone()),
            Box::new(self.metrics.last_finalization_interval.clone()),
        ]
    }
}

impl Metrics {
    fn new(last_finalization_time: IntGauge) -> Self {
        let last_eth_block_w_blob = IntGauge::with_opts(Opts::new(
            "last_eth_block_w_blob",
            "The height of the latest Ethereum block used for state submission.",
        ))
        .expect("last_eth_block_w_blob metric to be correctly configured");

        let last_finalization_interval = IntGauge::new(
            "seconds_from_earliest_submission_to_finalization",
            "The number of seconds from the earliest submission to finalization",
        )
        .expect(
            "seconds_from_earliest_submission_to_finalization gauge to be correctly configured",
        );

        Self {
            last_eth_block_w_blob,
            last_finalization_time,
            last_finalization_interval,
        }
    }
}
