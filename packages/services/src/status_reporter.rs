pub mod service {
    use crate::ports::storage::Storage;
    use serde::Serialize;

    use crate::Result;

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

    pub struct StatusReporter<Db> {
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

            let status = if last_submission_completed == Some(false) {
                Status::Committing
            } else {
                Status::Idle
            };

            Ok(StatusReport { status })
        }
    }
}

pub mod port {
    use crate::{types::BlockSubmission, Result};

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Send + Sync {
        async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    }
}
