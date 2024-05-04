use ports::storage::Storage;
use serde::Serialize;
use storage::Postgres;

use crate::errors::Result;

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

pub struct StatusReporter<Db = Postgres> {
    storage: Db,
}

impl<Db> StatusReporter<Db> {
    pub fn new(storage: Db) -> Self {
        Self { storage }
    }
}
impl<Db> StatusReporter<Db>
where
    Db: Storage,
{
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
    use std::sync::Arc;

    use ports::BlockSubmission;
    use rand::Rng;
    use storage::PostgresProcess;

    use super::*;

    #[tokio::test]
    async fn status_depends_on_last_submission() {
        let process = PostgresProcess::shared().await.unwrap();
        let test = |submission_status, expected_app_status| {
            let process = Arc::clone(&process);
            async move {
                // given
                let mut rng = rand::thread_rng();
                let db = process.create_random_db().await.unwrap();

                if let Some(is_completed) = submission_status {
                    let latest_submission = BlockSubmission {
                        completed: is_completed,
                        ..rng.gen()
                    };
                    db.insert(latest_submission).await.unwrap();
                }

                let status_reporter = StatusReporter::new(db);

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
