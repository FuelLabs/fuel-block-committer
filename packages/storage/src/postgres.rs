use ports::types::{BlockSubmission, Fragment, Submission, SubmissionTx, TransactionState};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::error::{Error, Result};
use crate::tables;

#[derive(Clone)]
pub struct Postgres {
    connection_pool: sqlx::Pool<sqlx::Postgres>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DbConfig {
    /// The hostname or IP address of the `PostgreSQL` server.
    pub host: String,
    /// The port number on which the `PostgreSQL` server is listening.
    pub port: u16,
    /// The username used to authenticate with the `PostgreSQL` server.
    pub username: String,
    /// The password used to authenticate with the `PostgreSQL` server.
    pub password: String,
    /// The name of the database to connect to on the `PostgreSQL` server.
    pub database: String,
    /// The maximum number of connections allowed in the connection pool.
    pub max_connections: u32,
    /// Whether to use SSL when connecting to the `PostgreSQL` server.
    pub use_ssl: bool,
}

impl Postgres {
    pub async fn connect(opt: &DbConfig) -> ports::storage::Result<Self> {
        let ssl_mode = if opt.use_ssl {
            sqlx::postgres::PgSslMode::Require
        } else {
            sqlx::postgres::PgSslMode::Disable
        };

        let options = PgConnectOptions::new()
            .ssl_mode(ssl_mode)
            .username(&opt.username)
            .password(&opt.password)
            .database(&opt.database)
            .host(&opt.host)
            .port(opt.port);

        let connection_pool = PgPoolOptions::new()
            .max_connections(opt.max_connections)
            .connect_with(options)
            .await
            .map_err(crate::error::Error::from)?;

        Ok(Self { connection_pool })
    }

    #[cfg(feature = "test-helpers")]
    pub fn db_name(&self) -> String {
        self.connection_pool
            .connect_options()
            .get_database()
            .expect("database name to be set")
            .to_owned()
    }

    #[cfg(feature = "test-helpers")]
    pub fn port(&self) -> u16 {
        self.connection_pool.connect_options().get_port()
    }

    /// Close only when shutting down the application. Will close the connection pool even if it is
    /// shared.
    pub async fn close(self) {
        self.connection_pool.close().await;
    }

    pub async fn migrate(&self) -> ports::storage::Result<()> {
        sqlx::migrate!()
            .run(&self.connection_pool)
            .await
            .map_err(crate::error::Error::from)?;
        Ok(())
    }

    #[cfg(feature = "test-helpers")]
    pub(crate) async fn execute(&self, query: &str) -> Result<()> {
        sqlx::query(query).execute(&self.connection_pool).await?;
        Ok(())
    }

    pub(crate) async fn _insert(&self, submission: BlockSubmission) -> crate::error::Result<()> {
        let row = tables::L1FuelBlockSubmission::from(submission);
        sqlx::query!(
            "INSERT INTO l1_fuel_block_submission (fuel_block_hash, fuel_block_height, completed, submittal_height) VALUES ($1, $2, $3, $4)",
            row.fuel_block_hash,
            row.fuel_block_height,
            row.completed,
            row.submittal_height
        ).execute(&self.connection_pool).await?;
        Ok(())
    }

    pub(crate) async fn _submission_w_latest_block(
        &self,
    ) -> crate::error::Result<Option<BlockSubmission>> {
        sqlx::query_as!(
            tables::L1FuelBlockSubmission,
            "SELECT * FROM l1_fuel_block_submission ORDER BY fuel_block_height DESC LIMIT 1"
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .map(BlockSubmission::try_from)
        .transpose()
    }

    pub(crate) async fn _set_submission_completed(
        &self,
        fuel_block_hash: [u8; 32],
    ) -> Result<BlockSubmission> {
        let updated_row = sqlx::query_as!(
            tables::L1FuelBlockSubmission,
            "UPDATE l1_fuel_block_submission SET completed = true WHERE fuel_block_hash = $1 RETURNING *",
            fuel_block_hash.as_slice(),
        ).fetch_optional(&self.connection_pool).await?;

        if let Some(row) = updated_row {
            Ok(row.try_into()?)
        } else {
            let hash = hex::encode(fuel_block_hash);
            Err(Error::Database(format!("Cannot set submission to completed! Submission of block: `{hash}` not found in DB.")))
        }
    }

    pub(crate) async fn _insert_state_submission(
        &self,
        state: Submission,
        fragments: Vec<Fragment>,
    ) -> Result<()> {
        if fragments.is_empty() {
            return Err(Error::Database(
                "cannot insert state with no fragments".to_string(),
            ));
        }

        let state_row = tables::L1Submission::from(state);
        let fragment_rows = fragments
            .into_iter()
            .map(tables::L1Fragment::from)
            .collect::<Vec<_>>();

        let mut transaction = self.connection_pool.begin().await?;

        // Insert the state submission
        let submission_id = sqlx::query!(
            "INSERT INTO l1_submissions (fuel_block_hash, fuel_block_height) VALUES ($1, $2) RETURNING id",
            state_row.fuel_block_hash,
            state_row.fuel_block_height
        )
        .fetch_one(&mut *transaction)
        .await?.id;

        // Insert the state fragments
        // TODO: optimize this
        for fragment_row in fragment_rows {
            sqlx::query!(
                "INSERT INTO l1_fragments (fragment_idx, submission_id, data, created_at) VALUES ($1, $2, $3, $4)",
                fragment_row.fragment_idx,
                submission_id,
                fragment_row.data,
                fragment_row.created_at
            )
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    pub(crate) async fn _get_unsubmitted_fragments(&self) -> Result<Vec<Fragment>> {
        let rows = sqlx::query_as!(
            // all fragments that are not in a transaction or are in a transaction that failed
            tables::L1Fragment,
            "SELECT l1_fragments.*
             FROM l1_fragments
             LEFT JOIN l1_transaction_fragments ON l1_fragments.id = l1_transaction_fragments.fragment_id
             LEFT JOIN l1_transactions ON l1_transaction_fragments.transaction_id = l1_transactions.id
             WHERE l1_transaction_fragments.fragment_id IS NULL OR l1_transactions.state = $1
             ORDER BY l1_fragments.created_at
             LIMIT 6;", // TODO: use blob limit
             TransactionState::Failed.into_i16()
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(Fragment::try_from);

        rows.collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _record_pending_tx(
        &self,
        tx_hash: [u8; 32],
        fragment_ids: Vec<u32>,
    ) -> Result<()> {
        let mut transaction = self.connection_pool.begin().await?;

        let transaction_id = sqlx::query!(
            "INSERT INTO l1_transactions (hash, state) VALUES ($1, $2) RETURNING id",
            tx_hash.as_slice(),
            TransactionState::Pending.into_i16(),
        )
        .fetch_one(&mut *transaction)
        .await?
        .id;

        for fragment_id in fragment_ids {
            sqlx::query!(
                "INSERT INTO l1_transaction_fragments (transaction_id, fragment_id) VALUES ($1, $2)",
                transaction_id,
                fragment_id as i64
            )
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    pub(crate) async fn _has_pending_txs(&self) -> Result<bool> {
        Ok(sqlx::query!(
            "SELECT EXISTS (SELECT 1 FROM l1_transactions WHERE state = $1) AS has_pending_transactions;",
            TransactionState::Pending.into_i16()
        )
        .fetch_one(&self.connection_pool)
        .await?
        .has_pending_transactions.unwrap_or(false))
    }

    pub(crate) async fn _get_pending_txs(&self) -> Result<Vec<SubmissionTx>> {
        sqlx::query_as!(
            tables::L1SubmissionTx,
            "SELECT * FROM l1_transactions WHERE state = $1",
            TransactionState::Pending.into_i16()
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(SubmissionTx::try_from)
        .collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _state_submission_w_latest_block(
        &self,
    ) -> crate::error::Result<Option<Submission>> {
        sqlx::query_as!(
            tables::L1Submission,
            "SELECT * FROM l1_submissions ORDER BY fuel_block_height DESC LIMIT 1"
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .map(Submission::try_from)
        .transpose()
    }

    pub(crate) async fn _update_submission_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()> {
        sqlx::query!(
            "UPDATE l1_transactions SET state = $1 WHERE hash = $2",
            state.into_i16(),
            hash.as_slice(),
        )
        .execute(&self.connection_pool)
        .await?;

        Ok(())
    }
}
