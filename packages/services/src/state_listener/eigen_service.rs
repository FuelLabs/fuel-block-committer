use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use tracing::info;

use crate::{
    types::{DispersalStatus, EigenDASubmission},
    Runner,
};

pub struct StateListener<EigenDA, Db> {
    eigenda_adapter: EigenDA,
    storage: Db,
    metrics: Metrics,
}

impl<EigenDA, Db> StateListener<EigenDA, Db> {
    pub fn new(l1_adapter: EigenDA, storage: Db, last_finalization_time_metric: IntGauge) -> Self {
        Self {
            eigenda_adapter: l1_adapter,
            storage,
            metrics: Metrics::new(last_finalization_time_metric),
        }
    }
}

impl<EigenDA, Db> StateListener<EigenDA, Db>
where
    EigenDA: crate::state_listener::port::eigen_da::Api,
    Db: crate::state_listener::port::Storage,
{
    async fn check_non_finalized(
        &self,
        non_finalized: Vec<EigenDASubmission>,
    ) -> crate::Result<()> {
        let mut changes = Vec::with_capacity(non_finalized.len());

        for submission in non_finalized {
            let status = self
                .eigenda_adapter
                .get_blob_status(submission.request_id.clone())
                .await?;
            let submission_id = submission.id.expect("submission id to be present") as u32;

            // skip if status didn't change
            if status == submission.status {
                continue;
            }

            match status {
                DispersalStatus::Processing => {
                    // log processing
                    tracing::info!(
                        "Processing submission with request_id: {}",
                        base64::encode(submission.request_id)
                    );
                    continue;
                }
                DispersalStatus::Confirmed => {
                    tracing::info!(
                        "Confirmed submission with request_id: {}",
                        base64::encode(submission.request_id)
                    );
                    changes.push((submission_id, DispersalStatus::Confirmed));
                }
                DispersalStatus::Finalized => {
                    tracing::info!(
                        "Finalized submission with request_id: {}",
                        base64::encode(submission.request_id)
                    );
                    changes.push((submission_id, DispersalStatus::Finalized));
                }
                _ => {
                    // log got bad status
                    tracing::info!(
                        "Unexpected status - submission with request_id: {}",
                        base64::encode(submission.request_id)
                    );
                    changes.push((submission_id, DispersalStatus::Failed));
                }
            }
        }

        self.storage.update_eigen_submissions(changes).await?;

        Ok(())
    }
}

impl<EigenDA, Db> Runner for StateListener<EigenDA, Db>
where
    EigenDA: crate::state_listener::port::eigen_da::Api + Send + Sync,
    Db: crate::state_listener::port::Storage,
{
    async fn run(&mut self) -> crate::Result<()> {
        let non_finalized = self.storage.get_non_finalized_eigen_submission().await?;

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

impl<L1, Db> RegistersMetrics for StateListener<L1, Db> {
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
