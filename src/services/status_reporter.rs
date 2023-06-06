use serde::Serialize;

use crate::{adapters::storage::Storage, common::EthTxStatus, errors::Result};

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
        let status_of_latest_submission = self
            .storage
            .submission_w_latest_block()
            .await?
            .map(|submission| submission.status);

        let status = if let Some(EthTxStatus::Pending) = status_of_latest_submission {
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
    use crate::adapters::storage::{EthTxSubmission, sqlite_db::SqliteDb};

    #[tokio::test]
    async fn status_depends_on_last_submission() {
        let doit = |submission_status, expected_app_status| {
            async move {
                // given
                let storage = SqliteDb::temporary().unwrap();
                let latest_submission = EthTxSubmission {
                    fuel_block_height: 1,
                    status: submission_status,
                    tx_hash: ethers::types::H256::default(),
                };
                storage.insert(latest_submission).await.unwrap();
                let status_reporter = StatusReporter::new(storage);

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

        doit(EthTxStatus::Pending, Status::Committing).await;
        doit(EthTxStatus::Aborted, Status::Idle).await;
        doit(EthTxStatus::Committed, Status::Idle).await;
    }
    #[tokio::test]
    async fn status_is_idle_if_no_submission() {
        // given
        let storage = SqliteDb::temporary().unwrap();
        let status_reporter = StatusReporter::new(storage);

        // when
        let status = status_reporter.current_status().await.unwrap();

        // then
        assert_eq!(
            status,
            StatusReport {
                status: Status::Idle
            }
        );
    }
}
