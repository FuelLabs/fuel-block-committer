use serde::Serialize;

use crate::{adapters::storage::Storage, errors::Result};

#[derive(Debug, Serialize, Default, PartialEq, Eq)]
pub struct StatusReport {
    pub status: Status,
}

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Committing,
}

pub struct StatusReporter {
    storage: Box<dyn Storage>,
}

impl StatusReporter {
    pub fn new(storage: impl Storage + 'static) -> Self {
        Self {
            storage: Box::new(storage),
        }
    }

    pub async fn current_status(&self) -> Result<StatusReport> {
        let last_submission_completed = self
            .storage
            .submission_w_latest_block()
            .await?
            .map(|submission| submission.completed);

        let status = if let Some(false) = last_submission_completed {
            Status::Committing
        } else {
            Status::Idle
        };

        Ok(StatusReport { status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{
        fuel_adapter::FuelBlock,
        storage::{postgresql::PostgresProcess, BlockSubmission},
    };

    #[tokio::test]
    async fn status_depends_on_last_submission() {
        let test = |submission_status, expected_app_status| {
            async move {
                // given
                let process = PostgresProcess::start().await.unwrap();
                if let Some(is_completed) = submission_status {
                    let latest_submission = BlockSubmission {
                        block: FuelBlock {
                            hash: Default::default(),
                            height: 1,
                        },
                        completed: is_completed,
                        ..BlockSubmission::random()
                    };
                    process.db().insert(latest_submission).await.unwrap();
                }

                let status_reporter = StatusReporter::new(process.db());

                // when
                let status = status_reporter.current_status().await.unwrap();

                // then
                assert_eq!(
                    status,
                    StatusReport {
                        status: expected_app_status
                    }
                );
            }
        };

        // has an entry, not completed
        test(Some(false), Status::Committing).await;
        // has an entry, completed
        test(Some(true), Status::Idle).await;
        // has no entry
        test(None, Status::Idle).await;
    }
}
