use ports::types::{BlockSubmission, StateFragment, StateFragmentId, StateSubmission};
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
}

impl Postgres {
    pub async fn connect(opt: &DbConfig) -> ports::storage::Result<Self> {
        let options = PgConnectOptions::new()
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

    pub(crate) async fn _insert_state(
        &self,
        state: StateSubmission,
        fragments: Vec<StateFragment>,
    ) -> Result<()> {
        if fragments.is_empty() {
            return Err(Error::Database(
                "Cannot insert state with no fragments".to_string(),
            ));
        }

        let state_row = tables::L1StateSubmission::from(state);
        let fragment_rows = fragments
            .into_iter()
            .map(tables::L1StateFragment::from)
            .collect::<Vec<_>>();

        let mut transaction = self.connection_pool.begin().await?;

        // Insert the state submission
        sqlx::query!(
            "INSERT INTO l1_state_submission (fuel_block_hash, fuel_block_height, completed) VALUES ($1, $2, $3)",
            state_row.fuel_block_hash,
            state_row.fuel_block_height,
            state_row.completed,
        )
        .execute(&mut *transaction)
        .await?;

        // Insert the state fragments
        // TODO: optimize this
        for fragment_row in fragment_rows {
            sqlx::query!(
                "INSERT INTO l1_state_fragment (fuel_block_hash, raw_data, fragment_index, completed) VALUES ($1, $2, $3, $4)",
                fragment_row.fuel_block_hash,
                fragment_row.raw_data,
                fragment_row.fragment_index,
                fragment_row.completed,
            )
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    pub(crate) async fn _get_unsubmitted_fragments(&self) -> Result<Vec<StateFragment>> {
        // TODO use blob limit
        let rows = sqlx::query_as!(
            tables::L1StateFragment,
            "SELECT * FROM l1_state_fragment WHERE completed = false LIMIT 6"
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(StateFragment::try_from);

        rows.collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _record_pending_tx(
        &self,
        tx_hash: [u8; 32],
        fragment_ids: Vec<StateFragmentId>,
    ) -> Result<()> {
        let mut transaction = self.connection_pool.begin().await?;

        sqlx::query!(
            "INSERT INTO l1_pending_transaction (transaction_hash) VALUES ($1)",
            tx_hash.as_slice()
        )
        .execute(&mut *transaction)
        .await?;

        for (block_hash, fragment_idx) in fragment_ids {
            sqlx::query!(
                "UPDATE l1_state_fragment SET transaction_hash = $1 WHERE fuel_block_hash = $2 AND fragment_index = $3",
                tx_hash.as_slice(),
                block_hash.as_slice(),
                fragment_idx as i64
            )
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    pub(crate) async fn _get_pending_txs(&self) -> Result<Vec<[u8; 32]>> {
        let rows = sqlx::query_as!(
            tables::L1PendingTransaction,
            "SELECT * FROM l1_pending_transaction"
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(<[u8; 32]>::try_from);

        rows.collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _state_submission_w_latest_block(
        &self,
    ) -> crate::error::Result<Option<StateSubmission>> {
        sqlx::query_as!(
            tables::L1StateSubmission,
            "SELECT * FROM l1_state_submission ORDER BY fuel_block_height DESC LIMIT 1"
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .map(StateSubmission::try_from)
        .transpose()
    }
}
