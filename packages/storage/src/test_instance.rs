use std::{
    borrow::Cow,
    ops::RangeInclusive,
    sync::{Arc, Weak},
};

use delegate::delegate;
use ports::{
    storage::{BundleFragment, FuelBlock, SequentialFuelBlocks, Storage},
    types::{
        BlockSubmission, DateTime, Fragment, L1Tx, NonEmpty, NonNegative, TransactionState, Utc,
    },
};
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    Image,
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
    pub async fn shared() -> ports::storage::Result<Arc<Self>> {
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

    pub async fn start() -> ports::storage::Result<Self> {
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
        .map_err(|e| ports::storage::Error::Database(format!("{e}")))?;

        Ok(Self {
            username,
            password,
            initial_db,
            container,
        })
    }

    pub async fn create_random_db(self: &Arc<Self>) -> ports::storage::Result<DbWithProcess> {
        let port = self
            .container
            .get_host_port_ipv4(5432)
            .await
            .map_err(|e| ports::storage::Error::Database(format!("{e}")))?;

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
        db.execute(&query).await?;

        config.database = db_name;

        let db = Postgres::connect(&config).await?;

        db.migrate().await?;

        Ok(DbWithProcess {
            db,
            _process: self.clone(),
        })
    }
}

#[derive(Clone)]
pub struct DbWithProcess {
    db: Postgres,
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

impl Storage for DbWithProcess {
    delegate! {
        to self.db {
            async fn insert(&self, submission: BlockSubmission) -> ports::storage::Result<()>;
            async fn submission_w_latest_block(&self) -> ports::storage::Result<Option<BlockSubmission>>;
            async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> ports::storage::Result<BlockSubmission>;
            async fn insert_blocks(&self, blocks: NonEmpty<FuelBlock>) -> ports::storage::Result<()>;
            async fn available_blocks(&self) -> ports::storage::Result<Option<RangeInclusive<u32>>>;
            async fn lowest_sequence_of_unbundled_blocks(
                &self,
                starting_height: u32,
                limit: usize,
            ) -> ports::storage::Result<Option<SequentialFuelBlocks>>;
            async fn insert_bundle_and_fragments(
                &self,
                block_range: RangeInclusive<u32>,
                fragments: NonEmpty<Fragment>,
            ) -> ports::storage::Result<()>;
            async fn record_pending_tx(
                &self,
                tx_hash: [u8; 32],
                fragment_ids: NonEmpty<NonNegative<i32>>,
            ) -> ports::storage::Result<()>;
            async fn get_pending_txs(&self) -> ports::storage::Result<Vec<L1Tx>>;
            async fn has_pending_txs(&self) -> ports::storage::Result<bool>;
            async fn oldest_nonfinalized_fragments(
                &self,
                starting_height: u32,
                limit: usize,
            ) -> ports::storage::Result<Vec<BundleFragment>>;
            async fn last_time_a_fragment_was_finalized(&self) -> ports::storage::Result<Option<DateTime<Utc>>>;
            async fn update_tx_state(&self, hash: [u8; 32], state: TransactionState) -> ports::storage::Result<()>;
        }
    }
}
