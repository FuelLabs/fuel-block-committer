pub mod service {
    use crate::{Result, Runner};
    use std::time::Duration;

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
    use crate::Result;
    use std::time::Duration;

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Send + Sync {
        async fn prune_entries_older_than(&self, duration: Duration) -> Result<u64>;
    }
}
