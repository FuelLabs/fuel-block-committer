use futures::{Stream, TryStreamExt};
use ports::types::{
    BlockSubmission, DateTime, L1Tx, NonNegative, StateSubmission, TransactionState, Utc,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::error::{Error, Result};
use crate::mappings::tables::{self, L1SubmissionTxState};

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
        todo!()
        // let row = tables::L1FuelBlockSubmission::from(submission);
        // sqlx::query!(
        //     "INSERT INTO l1_fuel_block_submission (fuel_block_hash, fuel_block_height, completed, submittal_height) VALUES ($1, $2, $3, $4)",
        //     row.fuel_block_hash,
        //     row.fuel_block_height,
        //     row.completed,
        //     row.submittal_height
        // ).execute(&self.connection_pool).await?;
        // Ok(())
    }

    pub(crate) async fn _all_blocks(&self) -> crate::error::Result<Vec<ports::storage::FuelBlock>> {
        sqlx::query_as!(
            tables::FuelBlock,
            "SELECT * FROM fuel_blocks ORDER BY height ASC"
        )
        .fetch_all(&self.connection_pool)
        .await
        .map_err(Error::from)?
        .into_iter()
        .map(ports::storage::FuelBlock::try_from)
        .collect()
    }

    pub(crate) async fn _block_roster(&self) -> crate::error::Result<ports::storage::BlockRoster> {
        let mut tx = self.connection_pool.begin().await?;
        let missing_block_heights = sqlx::query!(
            r#"WITH expected_heights AS (
                SELECT generate_series(
                    (SELECT MIN(height) FROM fuel_blocks), 
                    (SELECT MAX(height) FROM fuel_blocks)
                ) AS height
            )
            SELECT e.height
            FROM expected_heights e
            LEFT JOIN fuel_blocks fb ON fb.height = e.height
            WHERE fb.height IS NULL
            ORDER BY e.height ASC;  -- Explicitly enforce ascending order
            "#
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::from)?
        .into_iter()
        .map(|row| {
            let height = row.height.ok_or_else(|| {
                Error::Conversion("Missing height value. This is a bug".to_string())
            })?;

            u32::try_from(height)
                .map_err(|e| Error::Conversion(format!("db block height cannot fit in u32: {e}")))
        })
        .collect::<Result<Vec<_>>>()?;

        let highest_block_present =
            sqlx::query!("SELECT MAX(height) AS highest_block_present FROM fuel_blocks")
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::from)?
                .highest_block_present
                .map(|height| {
                    u32::try_from(height).map_err(|_| {
                        Error::Conversion(format!("db block height cannot fit in u32: {height}"))
                    })
                })
                .transpose()?;

        Ok(ports::storage::BlockRoster::new(
            missing_block_heights,
            highest_block_present,
        ))
    }

    pub(crate) async fn _insert_block(&self, block: ports::storage::FuelBlock) -> Result<()> {
        let row = tables::FuelBlock::from(block);
        sqlx::query!(
            "INSERT INTO fuel_blocks (hash, height, data) VALUES ($1, $2, $3)",
            row.hash,
            row.height,
            row.data
        )
        .execute(&self.connection_pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn _submission_w_latest_block(
        &self,
    ) -> crate::error::Result<Option<BlockSubmission>> {
        todo!()
        // sqlx::query_as!(
        //     tables::L1FuelBlockSubmission,
        //     "SELECT * FROM l1_fuel_block_submission ORDER BY fuel_block_height DESC LIMIT 1"
        // )
        // .fetch_optional(&self.connection_pool)
        // .await?
        // .map(BlockSubmission::try_from)
        // .transpose()
    }

    pub(crate) async fn _last_time_a_fragment_was_finalized(
        &self,
    ) -> crate::error::Result<Option<DateTime<Utc>>> {
        todo!()
        // let response = sqlx::query!(
        //     r#"SELECT
        //     MAX(l1_transactions.finalized_at) AS last_fragment_time
        // FROM
        //     l1_transaction_fragments
        // JOIN
        //     l1_transactions ON l1_transactions.id = l1_transaction_fragments.transaction_id
        // WHERE
        //     l1_transactions.state = $1;
        // "#,
        //     L1SubmissionTxState::FINALIZED_STATE
        // )
        // .fetch_optional(&self.connection_pool)
        // .await?
        // .and_then(|response| response.last_fragment_time);
        // Ok(response)
    }

    pub(crate) async fn _set_submission_completed(
        &self,
        fuel_block_hash: [u8; 32],
    ) -> Result<BlockSubmission> {
        todo!()
        // let updated_row = sqlx::query_as!(
        //     tables::L1FuelBlockSubmission,
        //     "UPDATE l1_fuel_block_submission SET completed = true WHERE fuel_block_hash = $1 RETURNING *",
        //     fuel_block_hash.as_slice(),
        // ).fetch_optional(&self.connection_pool).await?;
        //
        // if let Some(row) = updated_row {
        //     Ok(row.try_into()?)
        // } else {
        //     let hash = hex::encode(fuel_block_hash);
        //     Err(Error::Database(format!("Cannot set submission to completed! Submission of block: `{hash}` not found in DB.")))
        // }
    }

    pub(crate) async fn _insert_state_submission(&self, state: StateSubmission) -> Result<()> {
        todo!()
        // let L1StateSubmission {
        //     fuel_block_hash,
        //     fuel_block_height,
        //     data,
        //     ..
        // } = state.into();
        //
        // sqlx::query!(
        //     "INSERT INTO l1_submissions (fuel_block_hash, fuel_block_height, data) VALUES ($1, $2, $3)",
        //     fuel_block_hash,
        //     fuel_block_height,
        //     data
        // )
        // .execute(&self.connection_pool)
        // .await?;
        //
        // Ok(())
    }

    // pub(crate) fn _stream_unfinalized_segment_data(
    //     &self,
    // ) -> impl Stream<Item = Result<UnfinalizedSegmentData>> + '_ + Send {
    //     todo!()
    //     //     sqlx::query_as!(
    //     //         UnfinalizedSegmentData,
    //     //     r#"
    //     //     WITH finalized_fragments AS (
    //     //         SELECT
    //     //             s.fuel_block_height,
    //     //             s.id AS submission_id,
    //     //             octet_length(s.data) AS total_size,
    //     //             COALESCE(MAX(f.end_byte), 0) AS last_finalized_end_byte  -- Default to 0 if no fragments are finalized
    //     //         FROM l1_submissions s
    //     //         LEFT JOIN l1_fragments f ON f.submission_id = s.id
    //     //         LEFT JOIN l1_transactions t ON f.tx_id = t.id
    //     //         WHERE t.state = $1 OR t.state IS NULL
    //     //         GROUP BY s.fuel_block_height, s.id, s.data
    //     //     )
    //     //     SELECT
    //     //         ff.submission_id,
    //     //         COALESCE(ff.last_finalized_end_byte, 0) AS uncommitted_start,  -- Default to 0 if NULL
    //     //         ff.total_size AS uncommitted_end,  -- Non-inclusive end, which is the total size of the segment
    //     //         COALESCE(SUBSTRING(s.data FROM ff.last_finalized_end_byte + 1 FOR ff.total_size - ff.last_finalized_end_byte), ''::bytea) AS segment_data  -- Clip the data and default to an empty byte array if NULL
    //     //     FROM finalized_fragments ff
    //     //     JOIN l1_submissions s ON s.id = ff.submission_id
    //     //     ORDER BY ff.fuel_block_height ASC;
    //     //     "#,
    //     //     L1SubmissionTxState::FINALIZED_STATE as i16  // Only finalized transactions
    //     // )
    //     // .fetch(&self.connection_pool)
    //     // .map_err(Error::from)
    // }

    // pub(crate) async fn _record_pending_tx(
    //     &self,
    //     tx_hash: [u8; 32],
    //     fragments: Vec<StateFragment>,
    // ) -> Result<()> {
    //     todo!()
    //     // let mut transaction = self.connection_pool.begin().await?;
    //     //
    //     // let transaction_id = sqlx::query!(
    //     //     "INSERT INTO l1_transactions (hash, state) VALUES ($1, $2) RETURNING id",
    //     //     tx_hash.as_slice(),
    //     //     L1SubmissionTxState::PENDING_STATE
    //     // )
    //     // .fetch_one(&mut *transaction)
    //     // .await?
    //     // .id;
    //     //
    //     // for fragment in fragments {
    //     //     let tables::L1StateFragment {
    //     //         submission_id,
    //     //         start_byte,
    //     //         end_byte,
    //     //         ..
    //     //     } = tables::L1StateFragment::from(fragment);
    //     //
    //     //     sqlx::query!(
    //     //         "INSERT INTO l1_fragments (tx_id, submission_id, start_byte, end_byte) VALUES ($1, $2, $3, $4)",
    //     //         transaction_id,
    //     // submission_id,
    //     // start_byte,
    //     // end_byte
    //     //     )
    //     //     .execute(&mut *transaction)
    //     //     .await?;
    //     // }
    //     //
    //     // transaction.commit().await?;
    //     //
    //     // Ok(())
    // }

    pub(crate) async fn _has_pending_txs(&self) -> Result<bool> {
        todo!()
        // Ok(sqlx::query!(
        //     "SELECT EXISTS (SELECT 1 FROM l1_transactions WHERE state = $1) AS has_pending_transactions;",
        //     L1SubmissionTxState::PENDING_STATE
        // )
        // .fetch_one(&self.connection_pool)
        // .await?
        // .has_pending_transactions.unwrap_or(false))
    }

    pub(crate) async fn _get_pending_txs(&self) -> Result<Vec<L1Tx>> {
        todo!()
        // sqlx::query_as!(
        //     tables::L1SubmissionTx,
        //     "SELECT * FROM l1_transactions WHERE state = $1",
        //     L1SubmissionTxState::PENDING_STATE
        // )
        // .fetch_all(&self.connection_pool)
        // .await?
        // .into_iter()
        // .map(SubmissionTx::try_from)
        // .collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _state_submission_w_latest_block(
        &self,
    ) -> crate::error::Result<Option<StateSubmission>> {
        todo!()
        // sqlx::query_as!(
        //     tables::L1StateSubmission,
        //     "SELECT * FROM l1_submissions ORDER BY fuel_block_height DESC LIMIT 1"
        // )
        // .fetch_optional(&self.connection_pool)
        // .await?
        // .map(StateSubmission::try_from)
        // .transpose()
    }

    pub(crate) async fn _update_submission_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()> {
        todo!()
        // let L1SubmissionTxState {
        //     state,
        //     finalized_at,
        // } = state.into();
        // sqlx::query!(
        //     "UPDATE l1_transactions SET state = $1, finalized_at = $2 WHERE hash = $3",
        //     state,
        //     finalized_at,
        //     hash.as_slice(),
        // )
        // .execute(&self.connection_pool)
        // .await?;
        //
        // Ok(())
    }

    pub(crate) async fn insert_bundle_and_fragments(
        &self,
        bundle_blocks: &[[u8; 32]],
        fragments: Vec<Vec<u8>>,
    ) -> Result<()> {
        let mut tx = self.connection_pool.begin().await?;

        // Insert a new bundle
        let bundle_id = sqlx::query!(
            "INSERT INTO bundles (cancelled) VALUES ($1) RETURNING id",
            false // Initializing with `cancelled = false`
        )
        .fetch_one(&mut *tx)
        .await?
        .id;

        // Insert blocks into bundle_blocks table
        for block_hash in bundle_blocks {
            sqlx::query!(
                "INSERT INTO bundle_blocks (bundle_id, block_hash) VALUES ($1, $2)",
                bundle_id,
                block_hash
            )
            .execute(&mut *tx)
            .await?;
        }

        // Insert fragments associated with the bundle
        for (idx, fragment_data) in fragments.into_iter().enumerate() {
            sqlx::query!(
                "INSERT INTO l1_fragments (fragment_idx, data, bundle_id) VALUES ($1, $2, $3)",
                idx as i64,
                fragment_data,
                bundle_id
            )
            .execute(&mut *tx)
            .await?;
        }

        // Commit the transaction
        tx.commit().await?;

        Ok(())
    }

    pub(crate) async fn _block_available(&self, block_hash: &[u8; 32]) -> Result<bool> {
        let response = sqlx::query!(
            "SELECT EXISTS (SELECT 1 FROM fuel_blocks WHERE hash = $1) AS block_exists",
            block_hash
        )
        .fetch_one(&self.connection_pool)
        .await?;

        response.block_exists.ok_or_else(|| {
            Error::Database("Failed to determine if block exists. This is a bug".to_string())
        })
    }
}
