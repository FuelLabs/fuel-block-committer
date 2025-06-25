use metrics::{
    RegistersMetrics,
    prometheus::{IntGauge, core::Collector},
};

use crate::{
    Runner,
    types::{AsB64, DispersalStatus, EigenDASubmission},
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
        let mut last_finalized_request_id = None;

        for submission in non_finalized {
            let status = self
                .eigenda_adapter
                .get_blob_status(submission.request_id.clone())
                .await?;
            let submission_id = submission.id.expect("submission id to be present") as u32;

            tracing::info!(
                "Checking submission with request_id: {} - current status: {:?}",
                submission.as_base64(),
                status,
            );

            // skip if status didn't change
            if status == submission.status {
                tracing::info!(
                    "Skipping submission with request_id: {} - status didn't change",
                    submission.as_base64(),
                );
                continue;
            }

            match status {
                DispersalStatus::Processing => {
                    // log processing
                    tracing::info!(
                        "Processing submission with request_id: {}",
                        submission.as_base64(),
                    );
                    continue;
                }
                DispersalStatus::Confirmed => {
                    tracing::info!(
                        "Confirmed submission with request_id: {}",
                        submission.as_base64(),
                    );
                    changes.push((submission_id, DispersalStatus::Confirmed));
                }
                DispersalStatus::Finalized => {
                    tracing::info!(
                        "Finalized submission with request_id: {}",
                        submission.as_base64(),
                    );
                    last_finalized_request_id = Some(submission.request_id);
                    changes.push((submission_id, DispersalStatus::Finalized));
                }
                DispersalStatus::Other(other_status) => {
                    // log got bad status
                    tracing::info!(
                        "Unexpected status {other_status} - submission with request_id: {}",
                        submission.as_base64(),
                    );
                    changes.push((submission_id, DispersalStatus::Other(other_status)));
                }
                DispersalStatus::Failed => {
                    tracing::info!(
                        "Failed submission with request_id: {}",
                        submission.as_base64(),
                    );
                    changes.push((submission_id, DispersalStatus::Failed));
                }
            }
        }

        // get the last request that was finalized, and update metrics accordingly -
        if let Some(request_id) = last_finalized_request_id {
            let now = self.clock.now();
            self.metrics.last_finalization_time.set(now.timestamp());

            let earliest_submission_attempt = self
                .storage
                .earliest_eigen_submission_attempt(&request_id)
                .await?;

            self.metrics.last_finalization_interval.set(
                earliest_submission_attempt
                    .map(|earliest_submission_attempt| {
                        (now - earliest_submission_attempt).num_seconds()
                    })
                    .unwrap_or(0),
            );
        }

        self.storage.update_eigen_submissions(changes).await?;

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
        let non_finalized = self.storage.get_non_finalized_eigen_submission().await?;

        tracing::debug!(
            "Checking non-finalized submissions: {}",
            non_finalized.len()
        );

        if non_finalized.is_empty() {
            return Ok(());
        }

        self.check_non_finalized(non_finalized).await?;

        Ok(())
    }
}

#[derive(Clone)]
struct Metrics {
    last_finalization_time: IntGauge,
    last_finalization_interval: IntGauge,
}

impl<EigenDA, Db, Clock> RegistersMetrics for StateListener<EigenDA, Db, Clock> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.last_finalization_time.clone()),
            Box::new(self.metrics.last_finalization_interval.clone()),
        ]
    }
}

impl Metrics {
    fn new(last_finalization_time: IntGauge) -> Self {
        let last_finalization_interval = IntGauge::new(
            "seconds_from_earliest_submission_to_finalization",
            "The number of seconds from the earliest submission to finalization",
        )
        .expect(
            "seconds_from_earliest_submission_to_finalization gauge to be correctly configured",
        );

        Self {
            last_finalization_time,
            last_finalization_interval,
        }
    }
}
