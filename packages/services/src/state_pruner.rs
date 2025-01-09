pub mod service {
    use metrics::{
        prometheus::{core::Collector, IntGauge},
        RegistersMetrics,
    };

    use crate::{Result, Runner};
    use std::time::Duration;

    use super::create_int_gauge;

    pub struct StatePruner<Db, Clock> {
        storage: Db,
        clock: Clock,
        retention: Duration,
        metrics: Metrics,
    }

    impl<Db, Clock> StatePruner<Db, Clock> {
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
            let pre_pruning_table_sizes = self.storage.table_sizes().await?;
            let pruned_blocks_range = self
                .storage
                .prune_entries_older_than(self.clock.now() - self.retention)
                .await?;

            let post_pruning_table_sizes = self.storage.table_sizes().await?;
            let pruned = diff_dbg_str(&pre_pruning_table_sizes, &post_pruning_table_sizes);

            tracing::info!("Pruned: {pruned} with {pruned_blocks_range:?}");
            tracing::info!("Table sizes: {post_pruning_table_sizes:?}");

            self.metrics.observe_table_sizes(&post_pruning_table_sizes);

            Ok(())
        }
    }

    fn diff_dbg_str(old: &super::port::TableSizes, new: &super::port::TableSizes) -> String {
        [
            format!(
                "blob_transactions: {}, ",
                old.blob_transactions.saturating_sub(new.blob_transactions)
            ),
            format!(
                "fragments: {}, ",
                old.fragments.saturating_sub(new.fragments)
            ),
            format!(
                "transaction_fragments: {}, ",
                old.transaction_fragments
                    .saturating_sub(new.transaction_fragments)
            ),
            format!("bundles: {}, ", old.bundles.saturating_sub(new.bundles)),
            format!(
                "bundle_costs: {}, ",
                old.bundle_costs.saturating_sub(new.bundle_costs)
            ),
            format!("blocks: {}, ", old.blocks.saturating_sub(new.blocks)),
            format!(
                "contract_transactions: {}, ",
                old.contract_transactions
                    .saturating_sub(new.contract_transactions)
            ),
            format!(
                "contract_submissions: {}",
                old.contract_submissions
                    .saturating_sub(new.contract_submissions)
            ),
        ]
        .join("")
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
    struct TableSizes {
        blob_transactions: IntGauge,
        fragments: IntGauge,
        transaction_fragments: IntGauge,
        bundles: IntGauge,
        bundle_costs: IntGauge,
        blocks: IntGauge,
        contract_transactions: IntGauge,
        contract_submissions: IntGauge,
    }

    #[derive(Clone)]
    struct Metrics {
        sizes: TableSizes,
    }

    impl Metrics {
        fn observe_table_sizes(&self, sizes: &crate::state_pruner::port::TableSizes) {
            self.sizes
                .blob_transactions
                .set(sizes.blob_transactions.into());
            self.sizes.fragments.set(sizes.fragments.into());
            self.sizes
                .transaction_fragments
                .set(sizes.transaction_fragments.into());
            self.sizes.bundles.set(sizes.bundles.into());
            self.sizes.bundle_costs.set(sizes.bundle_costs.into());
            self.sizes.blocks.set(sizes.blocks.into());
            self.sizes
                .contract_transactions
                .set(sizes.contract_transactions.into());
            self.sizes
                .contract_submissions
                .set(sizes.contract_submissions.into());
        }
    }

    impl<Db, Clock> RegistersMetrics for StatePruner<Db, Clock> {
        fn metrics(&self) -> Vec<Box<dyn Collector>> {
            vec![
                Box::new(self.metrics.sizes.blob_transactions.clone()),
                Box::new(self.metrics.sizes.fragments.clone()),
                Box::new(self.metrics.sizes.transaction_fragments.clone()),
                Box::new(self.metrics.sizes.bundles.clone()),
                Box::new(self.metrics.sizes.bundle_costs.clone()),
                Box::new(self.metrics.sizes.blocks.clone()),
                Box::new(self.metrics.sizes.contract_transactions.clone()),
                Box::new(self.metrics.sizes.contract_submissions.clone()),
            ]
        }
    }

    impl Default for Metrics {
        fn default() -> Self {
            let blob_transactions =
                create_int_gauge("tsize_blob_transactions", "Blob transactions table size.");
            let fragments = create_int_gauge("tsize_fragments", "Fragments table size.");

            let transaction_fragments = create_int_gauge(
                "tsize_transaction_fragments",
                "Transaction fragments table size.",
            );
            let bundles = create_int_gauge("tsize_bundles", "Bundles table size.");
            let bundle_costs = create_int_gauge("tsize_bundle_costs", "Bundle costs table size.");
            let blocks = create_int_gauge("tsize_blocks", "Blocks table size.");
            let contract_transactions = create_int_gauge(
                "tsize_contract_transactions",
                "Contract transactions table size.",
            );
            let contract_submissions = create_int_gauge(
                "tsize_contract_submissions",
                "Contract submissions table size.",
            );

            let sizes = TableSizes {
                blob_transactions,
                fragments,
                transaction_fragments,
                bundles,
                bundle_costs,
                blocks,
                contract_transactions,
                contract_submissions,
            };

            Self { sizes }
        }
    }
}

pub mod port {
    use crate::{
        types::{DateTime, Utc},
        Result,
    };

    #[derive(Debug, Clone, PartialEq, PartialOrd)]
    pub struct TableSizes {
        pub blob_transactions: u32,
        pub fragments: u32,
        pub transaction_fragments: u32,
        pub bundles: u32,
        pub bundle_costs: u32,
        pub blocks: u32,
        pub contract_transactions: u32,
        pub contract_submissions: u32,
    }

    #[derive(Debug, Clone)]
    pub struct PrunedBlocksRange {
        pub start_height: u32,
        pub end_height: u32,
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Send + Sync {
        async fn prune_entries_older_than(&self, date: DateTime<Utc>) -> Result<PrunedBlocksRange>;
        async fn table_sizes(&self) -> Result<TableSizes>;
    }

    pub trait Clock {
        fn now(&self) -> DateTime<Utc>;
    }
}

fn create_int_gauge(name: &str, help: &str) -> metrics::prometheus::IntGauge {
    metrics::prometheus::IntGauge::with_opts(metrics::prometheus::Opts::new(name, help))
        .expect("is correct")
}
