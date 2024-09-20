use std::ops::RangeInclusive;

use itertools::Itertools;
use ports::{
    storage::{BundleFragment, SequentialFuelBlocks},
    types::{BlockSubmission, DateTime, NonEmptyVec, NonNegative, TransactionState, Utc},
};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    QueryBuilder,
};

use super::error::{Error, Result};
use crate::mappings::tables::{self, L1TxState};

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

    pub(crate) async fn _insert(&self, submission: BlockSubmission) -> Result<()> {
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

    pub(crate) async fn _oldest_nonfinalized_fragment(
        &self,
    ) -> Result<Option<ports::storage::BundleFragment>> {
        let fragment = sqlx::query_as!(
            tables::BundleFragment,
            r#"
                    SELECT f.id, f.bundle_id, f.idx, f.data
                    FROM l1_fragments f
                    LEFT JOIN l1_transaction_fragments tf ON tf.fragment_id = f.id
                    LEFT JOIN l1_transactions t ON t.id = tf.transaction_id
                    JOIN bundles b ON b.id = f.bundle_id
                    WHERE t.id IS NULL OR t.state = $1 -- Unsubmitted or failed fragments
                    ORDER BY b.start_height ASC, f.idx ASC
                    LIMIT 1;
                    "#,
            L1TxState::FAILED_STATE
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .map(TryFrom::try_from)
        .transpose()?;

        Ok(fragment)
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

    pub(crate) async fn _all_fragments(
        &self,
    ) -> crate::error::Result<Vec<ports::storage::BundleFragment>> {
        // TODO: segfault add cascading rules
        sqlx::query_as!(
            tables::BundleFragment,
            "SELECT * FROM l1_fragments ORDER BY idx ASC"
        )
        .fetch_all(&self.connection_pool)
        .await
        .map_err(Error::from)?
        .into_iter()
        .map(TryFrom::try_from)
        .collect()
    }

    pub(crate) async fn _available_blocks(
        &self,
    ) -> crate::error::Result<Option<RangeInclusive<u32>>> {
        let record = sqlx::query!("SELECT MIN(height) AS min, MAX(height) AS max FROM fuel_blocks")
            .fetch_one(&self.connection_pool)
            .await
            .map_err(Error::from)?;

        let Some((min, max)) = record.min.zip(record.max) else {
            return Ok(None);
        };

        let min = u32::try_from(min)
            .map_err(|_| Error::Conversion(format!("cannot convert height into u32: {min} ")))?;

        let max = u32::try_from(max)
            .map_err(|_| Error::Conversion(format!("cannot convert height into u32: {max} ")))?;

        Ok(Some(min..=max))
    }

    pub(crate) async fn _insert_blocks(
        &self,
        blocks: NonEmptyVec<ports::storage::FuelBlock>,
    ) -> Result<()> {
        // Currently: hash, height and data
        const FIELDS_PER_BLOCK: u16 = 3;
        /// The maximum number of bind parameters that can be passed to a single postgres query is
        /// u16::MAX. Sqlx panics if this limit is exceeded.
        const MAX_BLOCKS_PER_QUERY: usize = (u16::MAX / FIELDS_PER_BLOCK) as usize;

        let mut tx = self.connection_pool.begin().await?;

        let queries = blocks
            .into_iter()
            .map(tables::FuelBlock::from)
            .chunks(MAX_BLOCKS_PER_QUERY)
            .into_iter()
            .map(|chunk| {
                let mut query_builder =
                    QueryBuilder::new("INSERT INTO fuel_blocks (hash, height, data)");

                query_builder.push_values(chunk, |mut b, block| {
                    // update the constants above if you add/remove bindings
                    b.push_bind(block.hash)
                        .push_bind(block.height)
                        .push_bind(block.data);
                });

                query_builder
            })
            .collect_vec();

        for mut query in queries {
            query.build().execute(&mut *tx).await?;
        }

        tx.commit().await?;

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

    pub(crate) async fn _last_time_a_fragment_was_finalized(
        &self,
    ) -> crate::error::Result<Option<DateTime<Utc>>> {
        let response = sqlx::query!(
            r#"SELECT
            MAX(l1_transactions.finalized_at) AS last_fragment_time
        FROM
            l1_transaction_fragments
        JOIN
            l1_transactions ON l1_transactions.id = l1_transaction_fragments.transaction_id
        WHERE
            l1_transactions.state = $1;
        "#,
            L1TxState::FINALIZED_STATE
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .and_then(|response| response.last_fragment_time);
        Ok(response)
    }

    pub(crate) async fn _lowest_unbundled_blocks(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Option<SequentialFuelBlocks>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let response = sqlx::query_as!(
            tables::FuelBlock,
            r#"
            SELECT fb.*
            FROM fuel_blocks fb WHERE fb.height >= $1
            AND NOT EXISTS (
                SELECT 1
                FROM bundles b
                WHERE fb.height BETWEEN b.start_height AND b.end_height
            )
            ORDER BY fb.height LIMIT $2"#,
            i64::from(starting_height), // Parameter $1
            limit                       // Parameter $2
        )
        .fetch_all(&self.connection_pool)
        .await
        .map_err(Error::from)?;

        if response.is_empty() {
            return Ok(None);
        }

        let fuel_blocks = response
            .into_iter()
            .map(|b| b.try_into())
            .collect::<Result<Vec<ports::storage::FuelBlock>>>()?;

        Ok(Some(SequentialFuelBlocks::from_first_sequence(
            NonEmptyVec::try_from(fuel_blocks).expect("checked for emptyness"),
        )))
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

    pub(crate) async fn _record_pending_tx(
        &self,
        tx_hash: [u8; 32],
        fragment_id: NonNegative<i32>,
    ) -> Result<()> {
        let mut tx = self.connection_pool.begin().await?;

        let tx_id = sqlx::query!(
            "INSERT INTO l1_transactions (hash, state) VALUES ($1, $2) RETURNING id",
            tx_hash.as_slice(),
            L1TxState::PENDING_STATE
        )
        .fetch_one(&mut *tx)
        .await?
        .id;

        sqlx::query!(
            "INSERT INTO l1_transaction_fragments (transaction_id, fragment_id) VALUES ($1, $2)",
            tx_id,
            fragment_id.as_i32()
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn _has_pending_txs(&self) -> Result<bool> {
        Ok(sqlx::query!(
            "SELECT EXISTS (SELECT 1 FROM l1_transactions WHERE state = $1) AS has_pending_transactions;",
            L1TxState::PENDING_STATE
        )
        .fetch_one(&self.connection_pool)
        .await?
        .has_pending_transactions.unwrap_or(false))
    }

    pub(crate) async fn _get_pending_txs(&self) -> Result<Vec<ports::types::L1Tx>> {
        sqlx::query_as!(
            tables::L1Tx,
            "SELECT * FROM l1_transactions WHERE state = $1",
            L1TxState::PENDING_STATE
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(TryFrom::try_from)
        .collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _update_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()> {
        let L1TxState {
            state,
            finalized_at,
        } = state.into();
        sqlx::query!(
            "UPDATE l1_transactions SET state = $1, finalized_at = $2 WHERE hash = $3",
            state,
            finalized_at,
            hash.as_slice(),
        )
        .execute(&self.connection_pool)
        .await?;

        Ok(())
    }

    pub(crate) async fn _insert_bundle_and_fragments(
        &self,
        block_range: RangeInclusive<u32>,
        fragment_datas: NonEmptyVec<NonEmptyVec<u8>>,
    ) -> Result<NonEmptyVec<BundleFragment>> {
        let mut tx = self.connection_pool.begin().await?;

        let start = *block_range.start();
        let end = *block_range.end();

        // Insert a new bundle
        let bundle_id = sqlx::query!(
            "INSERT INTO bundles(start_height, end_height) VALUES ($1,$2) RETURNING id",
            i64::from(start),
            i64::from(end)
        )
        .fetch_one(&mut *tx)
        .await?
        .id;

        let mut fragments = Vec::with_capacity(fragment_datas.len().get());
        let bundle_id: NonNegative<i32> = bundle_id.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!("invalid bundle id received from db: {e}"))
        })?;

        // Insert fragments associated with the bundle
        for (idx, fragment_data) in fragment_datas.into_inner().into_iter().enumerate() {
            let idx = i32::try_from(idx).map_err(|_| {
                crate::error::Error::Conversion(format!("invalid idx for fragment: {idx}"))
            })?;
            let record = sqlx::query!(
                "INSERT INTO l1_fragments (idx, data, bundle_id) VALUES ($1, $2, $3) RETURNING id",
                idx,
                &fragment_data.inner(),
                bundle_id.as_i32()
            )
            .fetch_one(&mut *tx)
            .await?;

            let id = record.id.try_into().map_err(|e| {
                crate::error::Error::Conversion(format!(
                    "invalid fragment id received from db: {e}"
                ))
            })?;

            fragments.push(BundleFragment {
                id,
                idx: idx
                    .try_into()
                    .expect("guaranteed to be positive since it came from an usize"),
                bundle_id,
                data: fragment_data.clone(),
            });
        }

        // Commit the transaction
        tx.commit().await?;

        Ok(fragments.try_into().expect(
            "guaranteed to have at least one element since the data also came from a non empty vec",
        ))
    }

    pub(crate) async fn _is_block_available(&self, block_hash: &[u8; 32]) -> Result<bool> {
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
