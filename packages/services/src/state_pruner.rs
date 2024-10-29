pub mod service {
    use metrics::{
        prometheus::{core::Collector, IntGauge},
        RegistersMetrics,
    };
    use ports::types::Utc; //TODO: do not use from ports

    use crate::{Result, Runner};
    use std::time::Duration;

    use super::create_int_guage;

    pub struct StatePruner<Db> {
        storage: Db,
        retention: Duration,
        metrics: Metrics,
    }

    impl<Db> StatePruner<Db>
    where
        Db: crate::state_pruner::port::Storage,
    {
        pub fn new(storage: Db, retention: Duration) -> Self {
            Self {
                storage,
                retention,
                metrics: Metrics::default(),
            }
        }
    }

    impl<Db> StatePruner<Db>
    where
        Db: crate::state_pruner::port::Storage,
    {
        pub async fn prune(&self) -> Result<()> {
            let pruned = self
                .storage
                .prune_entries_older_than(Utc::now() - self.retention)
                .await?;

            dbg!(&pruned);

            self.metrics.observe_pruned(&pruned);

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

    #[derive(Clone)]
    struct Metrics {
        pruned_blob_transactions: IntGauge,
        pruned_fragments: IntGauge,
        pruned_bundles: IntGauge,
        pruned_blocks: IntGauge,
        pruned_contract_transactions: IntGauge,
        pruned_contract_submisions: IntGauge,
    }

    impl Metrics {
        fn observe_pruned(&self, pruned: &crate::state_pruner::port::Pruned) {
            self.pruned_blob_transactions
                .set(pruned.blob_transactions.into());
            self.pruned_fragments.set(pruned.fragments.into());
            self.pruned_bundles.set(pruned.bundles.into());
            self.pruned_blocks.set(pruned.blocks.into());
            self.pruned_contract_transactions
                .set(pruned.contract_transactions.into());
            self.pruned_contract_submisions
                .set(pruned.contract_submisions.into());
        }
    }

    impl<Db> RegistersMetrics for StatePruner<Db> {
        fn metrics(&self) -> Vec<Box<dyn Collector>> {
            vec![
                Box::new(self.metrics.pruned_blob_transactions.clone()),
                Box::new(self.metrics.pruned_fragments.clone()),
                Box::new(self.metrics.pruned_bundles.clone()),
                Box::new(self.metrics.pruned_blocks.clone()),
                Box::new(self.metrics.pruned_contract_transactions.clone()),
                Box::new(self.metrics.pruned_contract_submisions.clone()),
            ]
        }
    }

    impl Default for Metrics {
        fn default() -> Self {
            let pruned_blob_transactions = create_int_guage(
                "pruned_blob_transactions",
                "Number of pruned blob transactions.",
            );
            let pruned_fragments =
                create_int_guage("pruned_fragments", "Number of pruned fragments.");
            let pruned_bundles = create_int_guage("pruned_bundles", "Number of pruned bundles.");
            let pruned_blocks = create_int_guage("pruned_blocks", "Number of pruned blocks.");
            let pruned_contract_transactions = create_int_guage(
                "pruned_contract_transactions",
                "Number of pruned contract transactions.",
            );
            let pruned_contract_submisions = create_int_guage(
                "pruned_contract_submisions",
                "Number of pruned contract submissions.",
            );

            Self {
                pruned_blob_transactions,
                pruned_fragments,
                pruned_bundles,
                pruned_blocks,
                pruned_contract_transactions,
                pruned_contract_submisions,
            }
        }
    }
}

pub mod port {
    pub use ports::types::{DateTime, Utc}; //TODO: do not use from ports

    use crate::Result;

    #[derive(Debug, Clone)]
    pub struct Pruned {
        pub blob_transactions: u32,
        pub fragments: u32,
        pub bundles: u32,
        pub blocks: u32,
        pub contract_transactions: u32,
        pub contract_submisions: u32,
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Send + Sync {
        async fn prune_entries_older_than(&self, date: DateTime<Utc>) -> Result<Pruned>;
    }
}

fn create_int_guage(name: &str, help: &str) -> metrics::prometheus::IntGauge {
    metrics::prometheus::IntGauge::with_opts(metrics::prometheus::Opts::new(name, help))
        .expect("is correct")
}
