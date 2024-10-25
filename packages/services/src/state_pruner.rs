use std::time::Duration;

use crate::{Result, Runner};

pub mod service {
    use super::*;

    pub struct StatePruner<Db> {
        storage: Db,
        retention: Duration,
    }

    impl<Db> StatePruner<Db>
    where
        Db: crate::state_pruner::port::Storage,
    {
        pub fn new(storage: Db, retention: Duration) -> Self {
            Self { storage, retention }
        }
    }

    impl<Db> StatePruner<Db>
    where
        Db: crate::state_pruner::port::Storage,
    {
        pub async fn prune(&self) -> Result<()> {
            // TODO: @hal3e
            // - refactor ports to every service
            // - extend Postrgess to have Clock port and use this time when writing to the database
            // - extend Storage trait to include the prune method
            // - update Postgress to use new method
            // - call method here
            //
            // - configure timeout to run this service
            Ok(())
        }
    }

    impl<Db> Runner for StatePruner<Db>
    where
        Db: crate::state_pruner::port::Storage + Clone + Send + Sync,
    {
        async fn run(&mut self) -> Result<()> {
            self.prune().await
        }
    }
}

pub mod port {
    use super::*;

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Send + Sync {
        async fn prune_entries_older_than(&self, duration: Duration) -> Result<()>;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::state_pruner::service;
    use crate::test_utils;

    #[tokio::test]
    async fn prune_state() -> crate::Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let _ = setup.insert_fragments(0, 1).await;

        let tx_hash = [0; 32];
        setup.send_fragments(tx_hash).await;
        use storage::PostgresProcess;
        let db = PostgresProcess::shared()
            .await
            .unwrap()
            .create_random_db()
            .await
            .unwrap();

        let a = db.db_name();

        let mut pruner = service::StatePruner::new(setup.db(), Duration::from_secs(10));

        // when
        pruner.run().await?;

        // then
        // assert!(!setup.db().has_pending_txs().await?);
        // assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 0);
        // assert_eq!(
        //     setup
        //         .db()
        //         .last_time_a_fragment_was_finalized()
        //         .await?
        //         .unwrap(),
        //     now
        // );

        Ok(())
    }
}
