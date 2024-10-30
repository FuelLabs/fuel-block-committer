pub mod service {
    use metrics::{
        prometheus::{core::Collector, IntGauge},
        RegistersMetrics,
    };

    use crate::{Result, Runner};
    use std::time::Duration;

    use super::create_int_guage;

    pub struct StatePruner<Db, Clock> {
        storage: Db,
        clock: Clock,
        retention: Duration,
        metrics: Metrics,
    }

    impl<Db, Clock> StatePruner<Db, Clock>
    where
        Db: crate::state_pruner::port::Storage,
    {
        pub fn new(storage: Db, clock: Clock, retention: Duration) -> Self {
            Self {
                storage,
                clock,
                retention,
                metrics: Metrics::default(),
            }
        }
    }

    impl<Db, Clock> StatePruner<Db, Clock>
    where
        Db: crate::state_pruner::port::Storage,
        Clock: crate::state_pruner::port::Clock,
    {
        pub async fn prune(&self) -> Result<()> {
            let pruned = self
                .storage
                .prune_entries_older_than(self.clock.now() - self.retention)
                .await?;

            let table_sizes = self.storage.table_sizes().await?;

            self.metrics.observe_pruned(&pruned);
            self.metrics.observe_table_sizes(&table_sizes);

            // TODO: @hal3e
            // - add tests for contract tx and submissions
            // - refactor ports to every service
            // - configure timeout to run this service

            Ok(())
        }
    }

    impl<Db, Clock> Runner for StatePruner<Db, Clock>
    where
        Db: crate::state_pruner::port::Storage + Send + Sync,
        Clock: crate::state_pruner::port::Clock + Send + Sync,
    {
        async fn run(&mut self) -> Result<()> {
            self.prune().await
        }
    }

    #[derive(Clone)]
    struct Pruned {
        blob_transactions: IntGauge,
        fragments: IntGauge,
        transaction_fragments: IntGauge,
        bundles: IntGauge,
        blocks: IntGauge,
        contract_transactions: IntGauge,
        contract_submisions: IntGauge,
    }

    #[derive(Clone)]
    struct TableSizes {
        blob_transactions: IntGauge,
        fragments: IntGauge,
        transaction_fragments: IntGauge,
        bundles: IntGauge,
        blocks: IntGauge,
        contract_transactions: IntGauge,
        contract_submisions: IntGauge,
    }

    #[derive(Clone)]
    struct Metrics {
        pruned: Pruned,
        sizes: TableSizes,
    }

    impl Metrics {
        fn observe_pruned(&self, pruned: &crate::state_pruner::port::Pruned) {
            self.pruned
                .blob_transactions
                .set(pruned.blob_transactions.into());
            self.pruned.fragments.set(pruned.fragments.into());
            self.pruned
                .transaction_fragments
                .set(pruned.transaction_fragments.into());
            self.pruned.bundles.set(pruned.bundles.into());
            self.pruned.blocks.set(pruned.blocks.into());
            self.pruned
                .contract_transactions
                .set(pruned.contract_transactions.into());
            self.pruned
                .contract_submisions
                .set(pruned.contract_submisions.into());
        }

        fn observe_table_sizes(&self, sizes: &crate::state_pruner::port::TableSizes) {
            self.sizes
                .blob_transactions
                .set(sizes.blob_transactions.into());
            self.sizes.fragments.set(sizes.fragments.into());
            self.sizes
                .transaction_fragments
                .set(sizes.transaction_fragments.into());
            self.sizes.bundles.set(sizes.bundles.into());
            self.sizes.blocks.set(sizes.blocks.into());
            self.sizes
                .contract_transactions
                .set(sizes.contract_transactions.into());
            self.sizes
                .contract_submisions
                .set(sizes.contract_submisions.into());
        }
    }

    impl<Db, Clock> RegistersMetrics for StatePruner<Db, Clock> {
        fn metrics(&self) -> Vec<Box<dyn Collector>> {
            vec![
                Box::new(self.metrics.pruned.blob_transactions.clone()),
                Box::new(self.metrics.pruned.fragments.clone()),
                Box::new(self.metrics.pruned.transaction_fragments.clone()),
                Box::new(self.metrics.pruned.bundles.clone()),
                Box::new(self.metrics.pruned.blocks.clone()),
                Box::new(self.metrics.pruned.contract_transactions.clone()),
                Box::new(self.metrics.pruned.contract_submisions.clone()),
                Box::new(self.metrics.sizes.blob_transactions.clone()),
                Box::new(self.metrics.sizes.fragments.clone()),
                Box::new(self.metrics.sizes.transaction_fragments.clone()),
                Box::new(self.metrics.sizes.bundles.clone()),
                Box::new(self.metrics.sizes.blocks.clone()),
                Box::new(self.metrics.sizes.contract_transactions.clone()),
                Box::new(self.metrics.sizes.contract_submisions.clone()),
            ]
        }
    }

    impl Default for Metrics {
        fn default() -> Self {
            let blob_transactions = create_int_guage(
                "pruned_blob_transactions",
                "Number of pruned blob transactions.",
            );
            let fragments = create_int_guage("pruned_fragments", "Number of pruned fragments.");
            let transaction_fragments = create_int_guage(
                "pruned_transaction_fragments",
                "Number of pruned transaction fragments.",
            );
            let bundles = create_int_guage("pruned_bundles", "Number of pruned bundles.");
            let blocks = create_int_guage("pruned_blocks", "Number of pruned blocks.");
            let contract_transactions = create_int_guage(
                "pruned_contract_transactions",
                "Number of pruned contract transactions.",
            );
            let contract_submisions = create_int_guage(
                "pruned_contract_submisions",
                "Number of pruned contract submissions.",
            );

            let pruned = Pruned {
                blob_transactions,
                fragments,
                transaction_fragments,
                bundles,
                blocks,
                contract_transactions,
                contract_submisions,
            };

            let blob_transactions =
                create_int_guage("tsize_blob_transactions", "Blob transactions table size.");
            let fragments =
                create_int_guage("tsize_pruned_fragments", "Pruned fragments table size.");

            let transaction_fragments = create_int_guage(
                "tsize_transaction_fragments",
                "Transaction fragments table size.",
            );
            let bundles = create_int_guage("tsize_bundles", "Bundles table size.");
            let blocks = create_int_guage("tsize_blocks", "Blocks table size.");
            let contract_transactions = create_int_guage(
                "tsize_contract_transactions",
                "Contract transactions table size.",
            );
            let contract_submisions = create_int_guage(
                "tsize_contract_submisions",
                "Contract submissions table size.",
            );

            let sizes = TableSizes {
                blob_transactions,
                fragments,
                transaction_fragments,
                bundles,
                blocks,
                contract_transactions,
                contract_submisions,
            };

            Self { pruned, sizes }
        }
    }
}

pub mod port {
    pub use ports::types::{DateTime, Utc}; //TODO: @hal3e do not use from ports

    use crate::Result;

    #[derive(Debug, Clone)]
    pub struct Pruned {
        pub blob_transactions: u32,
        pub fragments: u32,
        pub transaction_fragments: u32,
        pub bundles: u32,
        pub blocks: u32,
        pub contract_transactions: u32,
        pub contract_submisions: u32,
    }

    #[derive(Debug, Clone)]
    pub struct TableSizes {
        pub blob_transactions: u32,
        pub fragments: u32,
        pub transaction_fragments: u32,
        pub bundles: u32,
        pub blocks: u32,
        pub contract_transactions: u32,
        pub contract_submisions: u32,
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Send + Sync {
        async fn prune_entries_older_than(&self, date: DateTime<Utc>) -> Result<Pruned>;
        async fn table_sizes(&self) -> Result<TableSizes>;
    }

    pub trait Clock {
        fn now(&self) -> DateTime<Utc>;
    }
}

fn create_int_guage(name: &str, help: &str) -> metrics::prometheus::IntGauge {
    metrics::prometheus::IntGauge::with_opts(metrics::prometheus::Opts::new(name, help))
        .expect("is correct")
}
