use std::{
    borrow::Cow,
    ops::RangeInclusive,
    sync::{Arc, Weak},
};

use delegate::delegate;
use serde::Serialize;
use services::{
    block_bundler::{self, port::UnbundledBlocks},
    block_committer, block_importer,
    types::{
        storage::{BundleFragment, SequentialFuelBlocks},
        BlockSubmission, BlockSubmissionTx, BundleCost, CompressedFuelBlock, DateTime,
        DispersalStatus, EigenDASubmission, Fragment, L1Tx, NonEmpty, NonNegative,
        TransactionCostUpdate, TransactionState, Utc,
    },
};
use sqlx::Executor;
use testcontainers::{
    Image,
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
};

use super::postgres::{DbConfig, Postgres};

struct PostgresImage {
    username: String,
    password: String,
    initial_db: String,
}

impl Image for PostgresImage {
    fn name(&self) -> &str {
        "postgres"
    }

    fn tag(&self) -> &str {
        "latest"
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        )]
    }

    fn expose_ports(&self) -> &[ContainerPort] {
        &const { [ContainerPort::Tcp(5432)] }
    }

    fn env_vars(
        &self,
    ) -> impl IntoIterator<Item = (impl Into<Cow<'_, str>>, impl Into<Cow<'_, str>>)> {
        [
            ("POSTGRES_USER", self.username.as_str()),
            ("POSTGRES_PASSWORD", self.password.as_str()),
            ("POSTGRES_DB", self.initial_db.as_str()),
        ]
    }
}

pub struct PostgresProcess {
    username: String,
    password: String,
    initial_db: String,
    container: testcontainers::ContainerAsync<PostgresImage>,
}

impl PostgresProcess {
    pub async fn shared() -> services::Result<Arc<Self>> {
        // If at some point no tests are running, the shared instance will be dropped. If
        // requested again, it will be recreated.
        // This is a workaround for the lack of a global setup/teardown for tests.

        static LOCK: tokio::sync::Mutex<Weak<PostgresProcess>> =
            tokio::sync::Mutex::const_new(Weak::new());
        let mut shared_process = LOCK.lock().await;

        let process = if let Some(running_process) = shared_process.upgrade() {
            running_process
        } else {
            let process = Arc::new(Self::start().await?);
            *shared_process = Arc::downgrade(&process);
            process
        };

        Ok(process)
    }

    pub async fn start() -> services::Result<Self> {
        let username = "username".to_string();
        let password = "password".to_string();
        let initial_db = "test".to_string();

        let container = PostgresImage {
            username: username.clone(),
            password: password.clone(),
            initial_db: initial_db.clone(),
        }
        .start()
        .await
        .map_err(|e| crate::error::Error::Database(format!("{e}")))?;

        Ok(Self {
            username,
            password,
            initial_db,
            container,
        })
    }

    pub async fn create_random_db(self: &Arc<Self>) -> services::Result<DbWithProcess> {
        let db = self.create_noschema_random_db().await?;

        db.db.migrate().await?;

        Ok(db)
    }

    pub async fn create_noschema_random_db(self: &Arc<Self>) -> services::Result<DbWithProcess> {
        let port = self
            .container
            .get_host_port_ipv4(5432)
            .await
            .map_err(|e| crate::error::Error::Database(format!("{e}")))?;

        let mut config = DbConfig {
            host: "localhost".to_string(),
            port,
            username: self.username.clone(),
            password: self.password.clone(),
            database: self.initial_db.clone(),
            max_connections: 5,
            use_ssl: false,
        };
        let db = Postgres::connect(&config).await?;

        let db_name = format!("test_db_{}", rand::random::<u32>());
        let query = format!("CREATE DATABASE {db_name}");
        db.pool()
            .execute(sqlx::query(&query))
            .await
            .map_err(crate::error::Error::from)?;

        config.database = db_name;

        let db = Postgres::connect(&config).await?;

        Ok(DbWithProcess {
            db,
            _process: self.clone(),
        })
    }
}

#[derive(Clone)]
pub struct DbWithProcess {
    pub db: Postgres,
    _process: Arc<PostgresProcess>,
}

impl DbWithProcess {
    delegate! {
        to self.db {
            pub fn db_name(&self) -> String;
            pub fn port(&self) -> u16;
        }
    }
}

impl services::state_pruner::port::Storage for DbWithProcess {
    delegate! {
        to self.db {
            async fn prune_entries_older_than(
                &self,
                date: DateTime<Utc>,
            ) -> services::Result<services::state_pruner::port::PrunedBlocksRange>;
            async fn table_sizes(
                &self,
            ) -> services::Result<services::state_pruner::port::TableSizes>;
        }
    }
}

impl services::state_listener::port::Storage for DbWithProcess {
    async fn get_non_finalized_txs(&self) -> services::Result<Vec<L1Tx>> {
        self.db._get_non_finalized_txs().await.map_err(Into::into)
    }

    async fn update_tx_states_and_costs(
        &self,
        selective_changes: Vec<([u8; 32], TransactionState)>,
        noncewide_changes: Vec<([u8; 32], u32, TransactionState)>,
        cost_per_tx: Vec<TransactionCostUpdate>,
    ) -> services::Result<()> {
        self.db
            ._update_tx_states_and_costs(selective_changes, noncewide_changes, cost_per_tx)
            .await
            .map_err(Into::into)
    }

    async fn has_pending_txs(&self) -> services::Result<bool> {
        self.db._has_pending_txs().await.map_err(Into::into)
    }

    async fn earliest_submission_attempt(
        &self,
        nonce: u32,
    ) -> services::Result<Option<DateTime<Utc>>> {
        self.db
            ._earliest_submission_attempt(nonce)
            .await
            .map_err(Into::into)
    }

    async fn get_non_finalized_eigen_submission(&self) -> services::Result<Vec<EigenDASubmission>> {
        unimplemented!();
    }
    async fn update_eigen_submissions(
        &self,
        changes: Vec<(u32, DispersalStatus)>,
    ) -> services::Result<()> {
        unimplemented!();
    }
}

impl block_importer::port::Storage for DbWithProcess {
    async fn missing_blocks(
        &self,
        starting_height: u32,
        current_height: u32,
    ) -> services::Result<Vec<RangeInclusive<u32>>> {
        self.db
            ._missing_blocks(starting_height, current_height)
            .await
            .map_err(Into::into)
    }

    async fn insert_blocks(&self, blocks: NonEmpty<CompressedFuelBlock>) -> services::Result<()> {
        self.db._insert_blocks(blocks).await.map_err(Into::into)
    }
}

impl block_bundler::port::Storage for DbWithProcess {
    async fn lowest_sequence_of_unbundled_blocks(
        &self,
        starting_height: u32,
        max_cumulative_bytes: u32,
    ) -> services::Result<Option<UnbundledBlocks>> {
        self.db
            ._lowest_unbundled_blocks(starting_height, max_cumulative_bytes)
            .await
            .map_err(Into::into)
    }
    async fn insert_bundle_and_fragments(
        &self,
        bundle_id: NonNegative<i32>,
        block_range: RangeInclusive<u32>,
        fragments: NonEmpty<Fragment>,
    ) -> services::Result<()> {
        self.db
            ._insert_bundle_and_fragments(bundle_id, block_range, fragments)
            .await
            .map_err(Into::into)
    }
    async fn next_bundle_id(&self) -> services::Result<NonNegative<i32>> {
        self.db._next_bundle_id().await.map_err(Into::into)
    }
}

impl block_committer::port::Storage for DbWithProcess {
    async fn record_block_submission(
        &self,
        submission_tx: BlockSubmissionTx,
        submission: BlockSubmission,
        created_at: DateTime<Utc>,
    ) -> services::Result<NonNegative<i32>> {
        self.db
            ._record_block_submission(submission_tx, submission, created_at)
            .await
            .map_err(Into::into)
    }
    async fn get_pending_block_submission_txs(
        &self,
        submission_id: NonNegative<i32>,
    ) -> services::Result<Vec<BlockSubmissionTx>> {
        self.db
            ._get_pending_block_submission_txs(submission_id)
            .await
            .map_err(Into::into)
    }
    async fn update_block_submission_tx(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> services::Result<BlockSubmission> {
        self.db
            ._update_block_submission_tx(hash, state)
            .await
            .map_err(Into::into)
    }
    async fn submission_w_latest_block(&self) -> services::Result<Option<BlockSubmission>> {
        self.db
            ._submission_w_latest_block()
            .await
            .map_err(Into::into)
    }
}

impl services::state_committer::port::Storage for DbWithProcess {
    async fn has_nonfinalized_txs(&self) -> services::Result<bool> {
        self.db._has_nonfinalized_txs().await.map_err(Into::into)
    }
    async fn last_time_a_fragment_was_finalized(&self) -> services::Result<Option<DateTime<Utc>>> {
        self.db
            ._last_time_a_fragment_was_finalized()
            .await
            .map_err(Into::into)
    }
    async fn record_pending_tx(
        &self,
        tx: L1Tx,
        fragment_ids: NonEmpty<NonNegative<i32>>,
        created_at: DateTime<Utc>,
    ) -> services::Result<()> {
        self.db
            ._record_pending_tx(tx, fragment_ids, created_at)
            .await
            .map_err(Into::into)
    }
    async fn oldest_nonfinalized_fragments(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> services::Result<Vec<BundleFragment>> {
        self.db
            ._oldest_nonfinalized_fragments(starting_height, limit)
            .await
            .map_err(Into::into)
    }
    async fn fragments_submitted_by_tx(
        &self,
        tx_hash: [u8; 32],
    ) -> services::Result<Vec<BundleFragment>> {
        self.db
            ._fragments_submitted_by_tx(tx_hash)
            .await
            .map_err(Into::into)
    }
    async fn get_latest_pending_txs(&self) -> services::Result<Option<services::types::L1Tx>> {
        self.db._get_latest_pending_txs().await.map_err(Into::into)
    }

    async fn latest_bundled_height(&self) -> services::Result<Option<u32>> {
        self.db._latest_bundled_height().await.map_err(Into::into)
    }

    async fn record_eigenda_submission(
        &self,
        submission: EigenDASubmission,
        fragment_id: i32,
        created_at: DateTime<Utc>,
    ) -> services::Result<()> {
        unimplemented!()
    }

    async fn oldest_unsubmitted_fragments(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> services::Result<Vec<BundleFragment>> {
        unimplemented!()
    }
}

impl services::status_reporter::port::Storage for DbWithProcess {
    async fn submission_w_latest_block(&self) -> services::Result<Option<BlockSubmission>> {
        self.db
            ._submission_w_latest_block()
            .await
            .map_err(Into::into)
    }
}

impl services::cost_reporter::port::Storage for DbWithProcess {
    async fn get_finalized_costs(
        &self,
        from_block_height: u32,
        limit: usize,
    ) -> services::Result<Vec<BundleCost>> {
        self.db
            ._get_finalized_costs(from_block_height, limit)
            .await
            .map_err(Into::into)
    }

    async fn get_latest_costs(&self, limit: usize) -> services::Result<Vec<BundleCost>> {
        self.db._get_latest_costs(limit).await.map_err(Into::into)
    }
}
