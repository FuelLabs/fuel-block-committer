use std::{collections::HashMap, ops::RangeInclusive};

use itertools::Itertools;
use metrics::{prometheus::IntGauge, RegistersMetrics};
use services::types::{
    storage::SequentialFuelBlocks, BlockSubmission, BlockSubmissionTx, BundleCost,
    CompressedFuelBlock, DateTime, Fragment, NonEmpty, NonNegative, TransactionCostUpdate,
    TransactionState, TryCollectNonEmpty, Utc,
};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgConnection, QueryBuilder,
};

use super::error::{Error, Result};
use crate::{
    mappings::tables::{self, L1TxState},
    postgres::tables::u128_to_bigdecimal,
};

#[derive(Debug, Clone)]
struct Metrics {
    height_of_latest_commitment: IntGauge,
    lowest_unbundled_height: IntGauge,
}

impl Default for Metrics {
    fn default() -> Self {
        let height_of_latest_commitment = IntGauge::new(
            "height_of_latest_commitment",
            "The height of the latest commitment",
        )
        .expect("height_of_latest_commitment gauge to be correctly configured");

        let lowest_unbundled_height = IntGauge::new(
            "lowest_unbundled_height",
            "The height of the lowest block unbundled block",
        )
        .expect("lowest_unbundled_height gauge to be correctly configured");

        Self {
            height_of_latest_commitment,
            lowest_unbundled_height,
        }
    }
}

#[derive(Clone)]
pub struct Postgres {
    connection_pool: sqlx::Pool<sqlx::Postgres>,
    metrics: Metrics,
}

impl RegistersMetrics for Postgres {
    fn metrics(&self) -> Vec<Box<dyn metrics::prometheus::core::Collector>> {
        vec![
            Box::new(self.metrics.height_of_latest_commitment.clone()),
            Box::new(self.metrics.lowest_unbundled_height.clone()),
        ]
    }
}

struct BundleCostUpdate {
    cost_contribution: u128,
    size_contribution: u64,
    latest_da_block_height: u64,
}

type BundleCostUpdates = HashMap<i32, BundleCostUpdate>;

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
    pub async fn connect(opt: &DbConfig) -> services::Result<Self> {
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

        Ok(Self {
            connection_pool,
            metrics: Metrics::default(),
        })
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

    pub async fn migrate(&self) -> services::Result<()> {
        sqlx::migrate!()
            .run(&self.connection_pool)
            .await
            .map_err(crate::error::Error::from)?;
        Ok(())
    }

    #[cfg(feature = "test-helpers")]
    pub(crate) fn pool(&self) -> sqlx::Pool<sqlx::Postgres> {
        self.connection_pool.clone()
    }

    pub(crate) async fn _record_block_submission(
        &self,
        submission_tx: BlockSubmissionTx,
        submission: BlockSubmission,
        created_at: DateTime<Utc>,
    ) -> crate::error::Result<NonNegative<i32>> {
        let mut transaction = self.connection_pool.begin().await?;

        let row = tables::L1FuelBlockSubmission::from(submission);
        let submission_id = sqlx::query!(
            "INSERT INTO l1_fuel_block_submission (fuel_block_hash, fuel_block_height, completed) VALUES ($1, $2, $3) RETURNING id",
            row.fuel_block_hash,
            row.fuel_block_height,
            row.completed,
        )
        .fetch_one(&mut *transaction)
        .await?
        .id;

        let id = NonNegative::try_from(submission_id)
            .map_err(|e| crate::error::Error::Conversion(e.to_string()))?;

        let row = tables::L1FuelBlockSubmissionTx::from(submission_tx);
        sqlx::query!(
            "INSERT INTO l1_transaction (hash, nonce, max_fee, priority_fee, submission_id, state, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            row.hash,
            row.nonce,
            row.max_fee,
            row.priority_fee,
            submission_id,
            i16::from(L1TxState::Pending),
            created_at,
        )
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;

        Ok(id)
    }

    pub(crate) async fn _get_pending_block_submission_txs(
        &self,
        submission_id: NonNegative<i32>,
    ) -> Result<Vec<BlockSubmissionTx>> {
        sqlx::query_as!(
            tables::L1FuelBlockSubmissionTx,
            "SELECT * FROM l1_transaction WHERE state = $1 AND submission_id = $2",
            i16::from(L1TxState::Pending),
            submission_id.as_i32()
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(BlockSubmissionTx::try_from)
        .collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _update_block_submission_tx(
        &self,
        tx_hash: [u8; 32],
        state: TransactionState,
    ) -> Result<BlockSubmission> {
        // tx shouldn't go back to pending
        assert_ne!(state, TransactionState::Pending);

        let mut transaction = self.connection_pool.begin().await?;

        let finalized_at = match &state {
            TransactionState::Finalized(date_time) => Some(*date_time),
            _ => None,
        };
        let state = i16::from(L1TxState::from(&state));
        // update the transaction state
        let tx_row = sqlx::query_as!(
            tables::L1FuelBlockSubmissionTx,
            "UPDATE l1_transaction SET state = $1, finalized_at = $2 WHERE hash = $3 RETURNING *",
            state,
            finalized_at,
            tx_hash.as_slice(),
        )
        .fetch_optional(&mut *transaction)
        .await?;

        let submission_id = if let Some(row) = tx_row {
            row.submission_id
        } else {
            let hash = hex::encode(tx_hash);
            return Err(Error::Database(format!(
                "Cannot update tx state! Tx with hash: `{hash}` not found in DB."
            )));
        };

        // set submission to completed
        let submission_row = sqlx::query_as!(
            tables::L1FuelBlockSubmission,
            "UPDATE l1_fuel_block_submission SET completed = true WHERE id = $1 RETURNING *",
            submission_id as i64
        )
        .fetch_optional(&mut *transaction)
        .await?
        .map(BlockSubmission::try_from)
        .transpose()?;

        let submission_row = if let Some(row) = submission_row {
            row
        } else {
            let hash = hex::encode(tx_hash);
            return Err(Error::Database(format!(
                "Cannot update tx state! Tx with hash: `{hash}` not found in DB."
            )));
        };

        transaction.commit().await?;

        Ok(submission_row)
    }

    pub(crate) async fn _oldest_nonfinalized_fragments(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Vec<services::types::storage::BundleFragment>> {
        let limit: i64 = limit.try_into().unwrap_or(i64::MAX);
        let fragments = sqlx::query_as!(
            tables::BundleFragment,
            r#"SELECT
        sub.id,
        sub.idx,
        sub.bundle_id,
        sub.data,
        sub.unused_bytes,
        sub.total_bytes,
        sub.start_height
    FROM (
        SELECT DISTINCT ON (f.id)
            f.*,
            b.start_height
        FROM l1_fragments f
        JOIN bundles b ON b.id = f.bundle_id
        WHERE
            b.end_height >= $2
            AND NOT EXISTS (
                SELECT 1
                FROM l1_transaction_fragments tf
                JOIN l1_blob_transaction t ON t.id = tf.transaction_id
                WHERE tf.fragment_id = f.id
                  AND t.state <> $1
            )
        ORDER BY
            f.id,
            b.start_height ASC,
            f.idx ASC
    ) AS sub
    ORDER BY
        sub.start_height ASC,
        sub.idx ASC
    LIMIT $3;
"#,
            i16::from(L1TxState::Failed),
            i64::from(starting_height),
            limit
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(TryFrom::try_from)
        .try_collect()?;

        Ok(fragments)
    }

    pub(crate) async fn _fragments_submitted_by_tx(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Vec<services::types::storage::BundleFragment>> {
        let fragments = sqlx::query_as!(
            tables::BundleFragment,
            r#"
                SELECT
                    f.*,
                    b.start_height
                FROM l1_fragments f
                JOIN l1_transaction_fragments tf ON tf.fragment_id = f.id
                JOIN l1_blob_transaction t ON t.id = tf.transaction_id
                JOIN bundles b ON b.id = f.bundle_id
                WHERE t.hash = $1
        "#,
            tx_hash.as_slice()
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(TryFrom::try_from)
        .try_collect()?;

        Ok(fragments)
    }

    pub(crate) async fn _missing_blocks(
        &self,
        starting_height: u32,
        current_height: u32,
    ) -> crate::error::Result<Vec<RangeInclusive<u32>>> {
        let heights: Vec<_> = sqlx::query!(
            r#"WITH all_heights AS (SELECT generate_series($1::BIGINT, $2::BIGINT) AS height)
                SELECT ah.height
                FROM all_heights ah
                LEFT JOIN fuel_blocks fb ON fb.height = ah.height
                WHERE fb.height IS NULL
                ORDER BY ah.height;"#,
            i64::from(starting_height),
            i64::from(current_height)
        )
        .fetch_all(&self.connection_pool)
        .await
        .map_err(Error::from)?
        .into_iter()
        .flat_map(|row| row.height)
        .map(|height| {
            u32::try_from(height).map_err(|_| {
                crate::error::Error::Conversion(format!("invalid block height: {height}"))
            })
        })
        .try_collect()?;

        Ok(create_ranges(heights))
    }

    pub(crate) async fn _insert_blocks(&self, blocks: NonEmpty<CompressedFuelBlock>) -> Result<()> {
        // Currently: height and data
        const FIELDS_PER_BLOCK: u16 = 2;
        /// The maximum number of bind parameters that can be passed to a single postgres query is
        /// u16::MAX. Sqlx panics if this limit is exceeded.
        const MAX_BLOCKS_PER_QUERY: usize = (u16::MAX / FIELDS_PER_BLOCK) as usize;

        let mut tx = self.connection_pool.begin().await?;

        let queries = blocks
            .into_iter()
            .map(tables::DBCompressedFuelBlock::from)
            .chunks(MAX_BLOCKS_PER_QUERY)
            .into_iter()
            .map(|chunk| {
                let mut query_builder = QueryBuilder::new("INSERT INTO fuel_blocks (height, data)");

                query_builder.push_values(chunk, |mut b, block| {
                    // update the constants above if you add/remove bindings
                    b.push_bind(block.height).push_bind(block.data);
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
        let submission = sqlx::query_as!(
            tables::L1FuelBlockSubmission,
            "SELECT * FROM l1_fuel_block_submission ORDER BY fuel_block_height DESC LIMIT 1"
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .map(BlockSubmission::try_from)
        .transpose()?;

        if let Some(submission) = &submission {
            self.metrics
                .height_of_latest_commitment
                .set(submission.block_height.into());
        }

        Ok(submission)
    }

    pub(crate) async fn _last_time_a_fragment_was_finalized(
        &self,
    ) -> crate::error::Result<Option<DateTime<Utc>>> {
        let response = sqlx::query!(
            r#"SELECT
            MAX(l1_blob_transaction.finalized_at) AS last_fragment_time
        FROM
            l1_transaction_fragments
        JOIN
            l1_blob_transaction ON l1_blob_transaction.id = l1_transaction_fragments.transaction_id
        WHERE
            l1_blob_transaction.state = $1;
        "#,
            i16::from(L1TxState::Finalized)
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .and_then(|response| response.last_fragment_time);

        Ok(response)
    }

    pub(crate) async fn _earliest_submission_attempt(
        &self,
        nonce: u32,
    ) -> Result<Option<DateTime<Utc>>> {
        let response = sqlx::query!(
            r#"SELECT
            MIN(l1_blob_transaction.created_at) AS earliest_tx_time
        FROM
            l1_blob_transaction
        WHERE
            l1_blob_transaction.nonce = $1;
        "#,
            nonce as i64
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .and_then(|response| response.earliest_tx_time);

        Ok(response)
    }

    pub(crate) async fn _lowest_unbundled_blocks(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Option<SequentialFuelBlocks>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);

        let response = sqlx::query_as!(
            tables::DBCompressedFuelBlock,
            r#"
            SELECT fb.* 
        FROM fuel_blocks fb 
        WHERE fb.height >= $1
        AND NOT EXISTS (
            SELECT 1 FROM bundles b 
            WHERE fb.height BETWEEN b.start_height AND b.end_height
            AND b.end_height >= $1
        ) 
        ORDER BY fb.height 
        LIMIT $2;
            "#,
            i64::from(starting_height), // Parameter $1
            limit                       // Parameter $2
        )
        .fetch_all(&self.connection_pool)
        .await
        .map_err(Error::from)?;

        let sequential_blocks = response
            .into_iter()
            .map(CompressedFuelBlock::try_from)
            .try_collect_nonempty()?
            .map(SequentialFuelBlocks::from_first_sequence);

        if let Some(sequential_blocks) = &sequential_blocks {
            let lowest_unbundled_height = *sequential_blocks.height_range().start();
            self.metrics
                .lowest_unbundled_height
                .set(lowest_unbundled_height.into());
        }

        Ok(sequential_blocks)
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
        submission_tx: services::types::L1Tx,
        fragment_ids: NonEmpty<NonNegative<i32>>,
        created_at: DateTime<Utc>,
    ) -> Result<()> {
        let mut tx = self.connection_pool.begin().await?;

        let row = tables::L1Tx::from(submission_tx);
        let tx_id = sqlx::query!(
            "INSERT INTO l1_blob_transaction (hash, state, nonce, max_fee, priority_fee, blob_fee, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
            row.hash,
            i16::from(L1TxState::Pending),
            row.nonce,
            row.max_fee,
            row.priority_fee,
            row.blob_fee,
            created_at
        )
        .fetch_one(&mut *tx)
        .await?
        .id;

        for id in fragment_ids {
            sqlx::query!(
            "INSERT INTO l1_transaction_fragments (transaction_id, fragment_id) VALUES ($1, $2)",
            tx_id,
            id.as_i32()
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn _has_pending_txs(&self) -> Result<bool> {
        Ok(sqlx::query!(
            "SELECT EXISTS (SELECT 1 FROM l1_blob_transaction WHERE state = $1) AS has_pending_transactions;",
            i16::from(L1TxState::Pending)
        )
        .fetch_one(&self.connection_pool)
        .await?
        .has_pending_transactions.unwrap_or(false))
    }

    pub(crate) async fn _has_nonfinalized_txs(&self) -> Result<bool> {
        Ok(sqlx::query!(
            "SELECT EXISTS (SELECT 1 FROM l1_blob_transaction WHERE state = $1 OR state = $2) AS has_nonfinalized_transactions;",
            i16::from(L1TxState::Pending),
            i16::from(L1TxState::IncludedInBlock)
        )
        .fetch_one(&self.connection_pool)
        .await?
        .has_nonfinalized_transactions.unwrap_or(false))
    }

    pub(crate) async fn _get_non_finalized_txs(&self) -> Result<Vec<services::types::L1Tx>> {
        sqlx::query_as!(
            tables::L1Tx,
            "SELECT * FROM l1_blob_transaction WHERE state = $1 or state = $2",
            i16::from(L1TxState::IncludedInBlock),
            i16::from(L1TxState::Pending)
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(TryFrom::try_from)
        .collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _get_latest_pending_txs(&self) -> Result<Option<services::types::L1Tx>> {
        sqlx::query_as!(
            tables::L1Tx,
            "SELECT * FROM l1_blob_transaction WHERE state = $1 ORDER BY created_at DESC LIMIT 1",
            i16::from(L1TxState::Pending)
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .map(TryFrom::try_from)
        .transpose()
    }

    pub(crate) async fn _latest_bundled_height(&self) -> Result<Option<u32>> {
        sqlx::query!("SELECT MAX(end_height) AS latest_bundled_height FROM bundles")
            .fetch_one(&self.connection_pool)
            .await?
            .latest_bundled_height
            .map(|height| {
                u32::try_from(height).map_err(|_| {
                    crate::error::Error::Conversion(format!("invalid block height: {height}"))
                })
            })
            .transpose()
    }

    pub(crate) async fn _update_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()> {
        let finalized_at = match &state {
            TransactionState::Finalized(date_time) => Some(*date_time),
            _ => None,
        };
        let state = i16::from(L1TxState::from(&state));

        sqlx::query!(
            "UPDATE l1_blob_transaction SET state = $1, finalized_at = $2 WHERE hash = $3",
            state,
            finalized_at,
            hash.as_slice(),
        )
        .execute(&self.connection_pool)
        .await?;

        Ok(())
    }

    pub(crate) async fn _update_tx_states_and_costs(
        &self,
        selective_changes: Vec<([u8; 32], TransactionState)>,
        noncewide_changes: Vec<([u8; 32], u32, TransactionState)>,
        cost_per_tx: Vec<TransactionCostUpdate>,
    ) -> Result<()> {
        let mut tx = self.connection_pool.begin().await?;

        self.update_transaction_states(&mut tx, &selective_changes, &noncewide_changes)
            .await?;

        self.update_costs(&mut tx, &cost_per_tx).await?;

        tx.commit().await?;

        Ok(())
    }

    async fn update_transaction_states(
        &self,
        tx: &mut PgConnection,
        selective_changes: &[([u8; 32], TransactionState)],
        noncewide_changes: &[([u8; 32], u32, TransactionState)],
    ) -> Result<()> {
        for (hash, state) in selective_changes {
            self.update_transaction_state(tx, hash, state).await?;
        }

        for (hash, nonce, state) in noncewide_changes {
            self.update_transactions_noncewide(tx, hash, *nonce, state)
                .await?;
        }

        Ok(())
    }

    async fn update_transaction_state(
        &self,
        tx: &mut PgConnection,
        hash: &[u8; 32],
        state: &TransactionState,
    ) -> Result<()> {
        let finalized_at = match state {
            TransactionState::Finalized(date_time) => Some(*date_time),
            _ => None,
        };
        let state_int = i16::from(L1TxState::from(state));

        sqlx::query!(
            "UPDATE l1_blob_transaction SET state = $1, finalized_at = $2 WHERE hash = $3",
            state_int,
            finalized_at,
            hash.as_slice(),
        )
        .execute(tx)
        .await?;

        Ok(())
    }

    async fn update_transactions_noncewide(
        &self,
        tx: &mut PgConnection,
        hash: &[u8; 32],
        nonce: u32,
        state: &TransactionState,
    ) -> Result<()> {
        let finalized_at = match state {
            TransactionState::Finalized(date_time) => Some(*date_time),
            _ => None,
        };
        let state_int = i16::from(L1TxState::from(state));

        // set all transactions with the same nonce to Failed
        sqlx::query!(
            "UPDATE l1_blob_transaction SET state = $1, finalized_at = $2 WHERE nonce = $3",
            i16::from(L1TxState::Failed),
            Option::<DateTime<Utc>>::None,
            nonce as i64,
        )
        .execute(&mut *tx)
        .await?;

        // update the specific transaction
        sqlx::query!(
            "UPDATE l1_blob_transaction SET state = $1, finalized_at = $2 WHERE hash = $3",
            state_int,
            finalized_at,
            hash.as_slice(),
        )
        .execute(tx)
        .await?;

        Ok(())
    }

    async fn update_costs(
        &self,
        tx: &mut PgConnection,
        cost_per_tx: &[TransactionCostUpdate],
    ) -> Result<()> {
        let bundle_updates = self.process_cost_updates(tx, cost_per_tx).await?;

        for (bundle_id, update) in bundle_updates {
            self.update_bundle_cost(tx, bundle_id, &update).await?;
        }

        Ok(())
    }

    async fn process_cost_updates(
        &self,
        tx: &mut PgConnection,
        cost_per_tx: &[TransactionCostUpdate],
    ) -> Result<BundleCostUpdates> {
        let mut bundle_updates: BundleCostUpdates = HashMap::new();

        for TransactionCostUpdate {
            tx_hash,
            total_fee,
            da_block_height,
        } in cost_per_tx
        {
            let rows = sqlx::query!(
                r#"
            SELECT
                f.bundle_id,
                SUM(f.total_bytes)::BIGINT       AS total_bytes,
                SUM(f.unused_bytes)::BIGINT      AS unused_bytes,
                COUNT(*)::BIGINT                 AS fragment_count
            FROM l1_blob_transaction t
            JOIN l1_transaction_fragments tf ON t.id = tf.transaction_id
            JOIN l1_fragments f              ON tf.fragment_id = f.id
            WHERE t.hash = $1
            GROUP BY f.bundle_id
            "#,
                tx_hash.as_slice()
            )
            .fetch_all(&mut *tx)
            .await?;

            let total_fragments_in_tx = rows
                .iter()
                .map(|r| r.fragment_count.unwrap_or(0) as u64)
                .sum::<u64>();

            for row in rows {
                let bundle_id = row.bundle_id;

                let frag_count_in_bundle = row.fragment_count.unwrap_or(0) as u64;
                let total_bytes = row.total_bytes.unwrap_or(0).max(0) as u64;
                let unused_bytes = row.unused_bytes.unwrap_or(0).max(0) as u64;
                let used_bytes = total_bytes.saturating_sub(unused_bytes);

                const PPM: u128 = 1_000_000;
                let fraction_in_ppm = if total_fragments_in_tx == 0 {
                    0u128
                } else {
                    u128::from(frag_count_in_bundle)
                        .saturating_mul(PPM)
                        .saturating_div(u128::from(total_fragments_in_tx))
                };

                let cost_contribution = fraction_in_ppm
                    .saturating_mul(*total_fee)
                    .saturating_div(PPM);

                let entry = bundle_updates.entry(bundle_id).or_insert(BundleCostUpdate {
                    cost_contribution: 0,
                    size_contribution: 0,
                    latest_da_block_height: 0,
                });

                entry.cost_contribution = entry.cost_contribution.saturating_add(cost_contribution);
                entry.size_contribution = entry.size_contribution.saturating_add(used_bytes);
                entry.latest_da_block_height = entry.latest_da_block_height.max(*da_block_height);
            }
        }

        Ok(bundle_updates)
    }

    async fn update_bundle_cost(
        &self,
        tx: &mut PgConnection,
        bundle_id: i32,
        update: &BundleCostUpdate,
    ) -> Result<()> {
        // Check if any fragment in the bundle is not associated with a finalized transaction
        let is_finalized = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) = 0 AS "is_finalized!"
            FROM l1_fragments f
            WHERE f.bundle_id = $1 AND NOT EXISTS (
                SELECT 1
                FROM l1_transaction_fragments tf
                JOIN l1_blob_transaction t ON tf.transaction_id = t.id
                WHERE tf.fragment_id = f.id AND t.state = $2
            )
            "#,
            bundle_id,
            i16::from(L1TxState::Finalized),
        )
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query!(
            r#"
            INSERT INTO bundle_cost (
                bundle_id, cost, size, da_block_height, is_finalized
            ) VALUES (
                $1, $2, $3, $4, $5
            )
            ON CONFLICT (bundle_id) DO UPDATE SET
                cost = bundle_cost.cost + EXCLUDED.cost,
                size = bundle_cost.size + EXCLUDED.size,
                da_block_height = EXCLUDED.da_block_height,
                is_finalized = EXCLUDED.is_finalized
            "#,
            bundle_id,
            u128_to_bigdecimal(update.cost_contribution),
            i64::try_from(update.size_contribution).unwrap(),
            i64::try_from(update.latest_da_block_height).unwrap(),
            is_finalized,
        )
        .execute(&mut *tx)
        .await?;

        Ok(())
    }

    pub(crate) async fn _get_finalized_costs(
        &self,
        from_block_height: u32,
        limit: usize,
    ) -> Result<Vec<BundleCost>> {
        sqlx::query_as!(
            tables::BundleCost,
            r#"
            SELECT
                bc.bundle_id,
                bc.cost,
                bc.size,
                bc.da_block_height,
                bc.is_finalized,
                b.start_height,
                b.end_height
            FROM
                bundle_cost bc
                JOIN bundles b ON bc.bundle_id = b.id
            WHERE
                b.end_height >= $1 AND bc.is_finalized = TRUE
            ORDER BY
                b.start_height ASC
            LIMIT $2
            "#,
            from_block_height as i64,
            limit as i64
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(BundleCost::try_from)
        .collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _get_latest_costs(&self, limit: usize) -> Result<Vec<BundleCost>> {
        sqlx::query_as!(
            tables::BundleCost,
            r#"
            SELECT
                bc.bundle_id,
                bc.cost,
                bc.size,
                bc.da_block_height,
                bc.is_finalized,
                b.start_height,
                b.end_height
            FROM
                bundle_cost bc
                JOIN bundles b ON bc.bundle_id = b.id
            WHERE
                bc.is_finalized = TRUE
            ORDER BY
                b.start_height DESC
            LIMIT $1
            "#,
            limit as i64
        )
        .fetch_all(&self.connection_pool)
        .await?
        .into_iter()
        .map(BundleCost::try_from)
        .collect::<Result<Vec<_>>>()
    }

    pub(crate) async fn _next_bundle_id(&self) -> Result<NonNegative<i32>> {
        let next_id = sqlx::query!("SELECT nextval(pg_get_serial_sequence('bundles', 'id'))")
            .fetch_one(&self.connection_pool)
            .await?
            .nextval
            .ok_or_else(|| {
                crate::error::Error::Database(
                    "next bundle id query returned NULL. This is a bug.".to_string(),
                )
            })?;

        let id = i32::try_from(next_id).map_err(|e| {
            crate::error::Error::Conversion(format!(
                "bundle id received from db is bigger than expected: {e}"
            ))
        })?;

        let non_negative = NonNegative::try_from(id).map_err(|e| {
            crate::error::Error::Conversion(format!("invalid bundle id received from db: {e}"))
        })?;

        Ok(non_negative)
    }

    pub(crate) async fn _insert_bundle_and_fragments(
        &self,
        bundle_id: NonNegative<i32>,
        block_range: RangeInclusive<u32>,
        fragments: NonEmpty<Fragment>,
    ) -> Result<()> {
        let mut tx = self.connection_pool.begin().await?;

        let start = *block_range.start();
        let end = *block_range.end();

        // Insert a new bundle and retrieve its ID
        let bundle_id = sqlx::query!(
            "INSERT INTO bundles(id, start_height, end_height) VALUES ($1, $2, $3) RETURNING id",
            bundle_id.get(),
            i64::from(start),
            i64::from(end)
        )
        .fetch_one(&mut *tx)
        .await?
        .id;

        let bundle_id: NonNegative<i32> = bundle_id.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!("invalid bundle id received from db: {}", e))
        })?;

        // Define constants for batching
        const FIELDS_PER_FRAGMENT: u16 = 5; // idx, data, bundle_id, unused_bytes, total_bytes
        const MAX_FRAGMENTS_PER_QUERY: usize = (u16::MAX / FIELDS_PER_FRAGMENT) as usize;

        // Prepare fragments for insertion
        let fragment_rows = fragments
            .into_iter()
            .enumerate()
            .map(|(idx, fragment)| {
                let idx = i32::try_from(idx).map_err(|_| {
                    crate::error::Error::Conversion(format!("invalid idx for fragment: {}", idx))
                })?;
                Ok((
                    idx,
                    Vec::from(fragment.data),
                    bundle_id.as_i32(),
                    i64::from(fragment.unused_bytes),
                    i64::from(fragment.total_bytes.get()),
                ))
            })
            .collect::<Result<Vec<_>>>()?;

        // Batch insert fragments
        let queries = fragment_rows
            .into_iter()
            .chunks(MAX_FRAGMENTS_PER_QUERY)
            .into_iter()
            .map(|chunk| {
                let mut query_builder = QueryBuilder::new(
                    "INSERT INTO l1_fragments (idx, data, bundle_id, unused_bytes, total_bytes)",
                );

                query_builder.push_values(chunk, |mut b, values| {
                    b.push_bind(values.0);
                    b.push_bind(values.1);
                    b.push_bind(values.2);
                    b.push_bind(values.3);
                    b.push_bind(values.4);
                });

                query_builder
            })
            .collect::<Vec<_>>();

        // Execute all fragment insertion queries
        for mut query in queries {
            query.build().execute(&mut *tx).await?;
        }

        // Commit the transaction
        tx.commit().await?;

        Ok(())
    }

    pub(crate) async fn _prune_entries_older_than(
        &self,
        date: DateTime<Utc>,
    ) -> Result<services::state_pruner::port::PrunedBlocksRange> {
        let mut transaction = self.connection_pool.begin().await?;

        let response = sqlx::query!(
            r#"
            WITH

            -- Delete from l1_blob_transaction
            deleted_blob_transactions AS (
                DELETE FROM l1_blob_transaction
                WHERE created_at < $1
                RETURNING id
            ),

            -- Delete from l1_transaction_fragments
            deleted_transaction_fragments AS (
                DELETE FROM l1_transaction_fragments
                WHERE transaction_id IN (SELECT id FROM deleted_blob_transactions)
                RETURNING transaction_id, fragment_id
            ),

            -- Build updated_transaction_fragments that represent the state after deletions
            updated_transaction_fragments AS (
                SELECT fragment_id FROM l1_transaction_fragments
                WHERE transaction_id NOT IN (SELECT transaction_id FROM deleted_transaction_fragments)
            ),

            -- Delete fragments that are not referenced by any other transaction
            deleted_fragments AS (
                DELETE FROM l1_fragments f
                WHERE id IN (SELECT fragment_id FROM deleted_transaction_fragments)
                  AND NOT EXISTS (
                      SELECT 1
                      FROM updated_transaction_fragments tf
                      WHERE tf.fragment_id = f.id
                  )
                RETURNING id, bundle_id
            ),

            -- Step 4: Build updated_fragments that represent the state after deletions
            updated_fragments AS (
                SELECT bundle_id
                FROM l1_fragments
                WHERE id NOT IN (SELECT id FROM deleted_fragments)
            ),

            -- Delete unreferenced bundles and collect start and end heights
            deleted_bundles AS (
                DELETE FROM bundles b
                WHERE id IN (SELECT bundle_id FROM deleted_fragments)
                  AND NOT EXISTS (
                      SELECT 1
                      FROM updated_fragments f
                      WHERE f.bundle_id = b.id
                  )
                RETURNING start_height, end_height, id
            ),

            -- Delete unreferenced bundle costs
            deleted_bundle_costs AS (
                DELETE FROM bundle_cost bc
                WHERE bundle_id IN (SELECT id FROM deleted_bundles)
            ),

            -- Delete corresponding fuel_blocks entries
            deleted_fuel_blocks AS (
                DELETE FROM fuel_blocks fb
                WHERE EXISTS (
                    SELECT 1
                    FROM deleted_bundles db
                    WHERE fb.height BETWEEN db.start_height AND db.end_height
                )
            ),

            -- Delete from l1_transaction
            deleted_transactions AS (
                DELETE FROM l1_transaction
                WHERE created_at < $1
                RETURNING id, submission_id
            ),

            -- Build updated_transactions that represent the state after deletions
            updated_transactions AS (
                SELECT submission_id FROM l1_transaction
                WHERE id NOT IN (SELECT id FROM deleted_transactions)
            ),

            -- Delete from l1_fuel_block_submission
            deleted_submissions AS (
                DELETE FROM l1_fuel_block_submission bs
                WHERE id IN (SELECT submission_id FROM deleted_transactions)
                  AND NOT EXISTS (
                      SELECT 1
                      FROM updated_transactions t
                      WHERE t.submission_id = bs.id
                  )
            )

            SELECT
                MIN(start_height) AS start_height,
                MAX(end_height) AS end_height
            FROM deleted_bundles;
            "#,
           date
        )
        .fetch_one(&mut *transaction)
        .await?;

        transaction.commit().await?;

        Ok(services::state_pruner::port::PrunedBlocksRange {
            start_height: response.start_height.unwrap_or_default() as u32,
            end_height: response.end_height.unwrap_or_default() as u32,
        })
    }

    pub(crate) async fn _table_sizes(&self) -> Result<services::state_pruner::port::TableSizes> {
        let response = sqlx::query!(
            r#"
            SELECT
                (SELECT COUNT(*) FROM l1_blob_transaction) AS size_blob_transactions,
                (SELECT COUNT(*) FROM l1_transaction_fragments) AS size_transaction_fragments,
                (SELECT COUNT(*) FROM l1_fragments) AS size_fragments,
                (SELECT COUNT(*) FROM bundles) AS size_bundles,
                (SELECT COUNT(*) FROM bundle_cost) AS size_bundle_costs,
                (SELECT COUNT(*) FROM fuel_blocks) AS size_fuel_blocks,
                (SELECT COUNT(*) FROM l1_transaction) AS size_contract_transactions,
                (SELECT COUNT(*) FROM l1_fuel_block_submission) AS size_contract_submissions
            "#,
        )
        .fetch_one(&self.connection_pool)
        .await?;

        Ok(services::state_pruner::port::TableSizes {
            blob_transactions: response.size_blob_transactions.unwrap_or_default() as u32,
            fragments: response.size_fragments.unwrap_or_default() as u32,
            transaction_fragments: response.size_transaction_fragments.unwrap_or_default() as u32,
            bundles: response.size_bundles.unwrap_or_default() as u32,
            bundle_costs: response.size_bundle_costs.unwrap_or_default() as u32,
            blocks: response.size_fuel_blocks.unwrap_or_default() as u32,
            contract_transactions: response.size_contract_transactions.unwrap_or_default() as u32,
            contract_submissions: response.size_contract_submissions.unwrap_or_default() as u32,
        })
    }
}

fn create_ranges(heights: Vec<u32>) -> Vec<RangeInclusive<u32>> {
    // db should take care of it always being ASC sorted and unique, but just in case it doesn't
    // hurt to dedupe and sort here
    heights
        .into_iter()
        .unique()
        .sorted_unstable()
        .enumerate()
        .chunk_by(|(i, height)| {
            // consecutive heights will give the same number when subtracted from their indexes
            // heights( 5, 6, 7) -> ( 5-0, 6-1, 7-2) = ( 5, 5, 5 )
            height
                .checked_sub(*i as u32)
                .expect("cannot underflow since elements are sorted and `height` is always >= `i` ")
        })
        .into_iter()
        .map(|(_key, group)| {
            let mut group_iter = group.map(|(_, h)| h);
            let start = group_iter.next().expect("group cannot be empty");
            let end = group_iter.last().unwrap_or(start);
            start..=end
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::Path};

    use rand::Rng;
    use services::types::{CollectNonEmpty, Fragment, L1Tx, TransactionState};
    use sqlx::{Executor, PgPool, Row};
    use tokio::time::Instant;

    use super::*;
    use crate::test_instance;

    #[tokio::test]
    async fn test_second_migration_applies_successfully() {
        let db = test_instance::PostgresProcess::shared()
            .await
            .expect("Failed to initialize PostgresProcess")
            .create_noschema_random_db()
            .await
            .expect("Failed to create random test database");

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let migrations_path = Path::new(manifest_dir).join("migrations");

        async fn apply_migration(pool: &sqlx::Pool<sqlx::Postgres>, path: &Path) {
            let sql = fs::read_to_string(path)
                .map_err(|e| format!("Failed to read migration file {:?}: {}", path, e))
                .unwrap();
            pool.execute(sqlx::raw_sql(&sql)).await.unwrap();
        }

        // -----------------------
        // Apply Initial Migration
        // -----------------------
        let initial_migration_path = migrations_path.join("0001_initial.up.sql");
        apply_migration(&db.db.pool(), &initial_migration_path).await;

        // Insert sample data into initial tables

        let fuel_block_hash = vec![0u8; 32];
        let insert_l1_submissions = r#"
        INSERT INTO l1_submissions (fuel_block_hash, fuel_block_height)
        VALUES ($1, $2)
        RETURNING id
    "#;
        let row = sqlx::query(insert_l1_submissions)
            .bind(&fuel_block_hash)
            .bind(1000i64)
            .fetch_one(&db.db.pool())
            .await
            .unwrap();
        let submission_id: i32 = row.try_get("id").unwrap();

        let insert_l1_fuel_block_submission = r#"
        INSERT INTO l1_fuel_block_submission (fuel_block_hash, fuel_block_height, completed, submittal_height)
        VALUES ($1, $2, $3, $4)
    "#;
        sqlx::query(insert_l1_fuel_block_submission)
            .bind(&fuel_block_hash)
            .bind(1000i64)
            .bind(true)
            .bind(950i64)
            .execute(&db.db.pool())
            .await
            .unwrap();

        // Insert into l1_transactions
        let tx_hash = vec![1u8; 32];
        let insert_l1_transactions = r#"
        INSERT INTO l1_transactions (hash, state)
        VALUES ($1, $2)
        RETURNING id
    "#;
        let row = sqlx::query(insert_l1_transactions)
            .bind(&tx_hash)
            .bind(0i16)
            .fetch_one(&db.db.pool())
            .await
            .unwrap();
        let transaction_id: i32 = row.try_get("id").unwrap();

        // Insert into l1_fragments
        let fragment_data = vec![2u8; 10];
        let insert_l1_fragments = r#"
        INSERT INTO l1_fragments (fragment_idx, submission_id, data)
        VALUES ($1, $2, $3)
        RETURNING id
    "#;
        let row = sqlx::query(insert_l1_fragments)
            .bind(0i64)
            .bind(submission_id)
            .bind(&fragment_data)
            .fetch_one(&db.db.pool())
            .await
            .unwrap();
        let fragment_id: i32 = row.try_get("id").unwrap();

        // Insert into l1_transaction_fragments
        let insert_l1_transaction_fragments = r#"
        INSERT INTO l1_transaction_fragments (transaction_id, fragment_id)
        VALUES ($1, $2)
    "#;
        sqlx::query(insert_l1_transaction_fragments)
            .bind(transaction_id)
            .bind(fragment_id)
            .execute(&db.db.pool())
            .await
            .unwrap();

        // ------------------------
        // Apply Second Migration
        // ------------------------
        let second_migration_path = migrations_path.join("0002_better_fragmentation.up.sql");
        apply_migration(&db.db.pool(), &second_migration_path).await;

        // ------------------------
        // Verification Steps
        // ------------------------

        // Function to check table existence
        async fn table_exists(pool: &PgPool, table_name: &str) -> bool {
            let query = r#"
            SELECT EXISTS (
                SELECT FROM information_schema.tables
                WHERE table_schema = 'public'
                AND table_name = $1
            )
        "#;
            let row = sqlx::query(query)
                .bind(table_name)
                .fetch_one(pool)
                .await
                .expect("Failed to execute table_exists query");
            row.try_get::<bool, _>(0).unwrap_or(false)
        }

        // Function to check column existence and type
        async fn column_info(pool: &PgPool, table_name: &str, column_name: &str) -> Option<String> {
            let query = r#"
            SELECT data_type
            FROM information_schema.columns
            WHERE table_name = $1 AND column_name = $2
        "#;
            let row = sqlx::query(query)
                .bind(table_name)
                .bind(column_name)
                .fetch_optional(pool)
                .await
                .expect("Failed to execute column_info query");
            row.map(|row| row.try_get("data_type").unwrap_or_default())
        }

        let fuel_blocks_exists = table_exists(&db.db.pool(), "fuel_blocks").await;
        assert!(fuel_blocks_exists, "fuel_blocks table does not exist");

        let bundles_exists = table_exists(&db.db.pool(), "bundles").await;
        assert!(bundles_exists, "bundles table does not exist");

        async fn check_columns(pool: &PgPool, table: &str, column: &str, expected_type: &str) {
            let info = column_info(pool, table, column).await;
            assert!(
                info.is_some(),
                "Column '{}' does not exist in table '{}'",
                column,
                table
            );
            let data_type = info.unwrap();
            assert_eq!(
                data_type, expected_type,
                "Column '{}' in table '{}' has type '{}', expected '{}'",
                column, table, data_type, expected_type
            );
        }

        // Check that 'l1_fragments' has new columns
        check_columns(&db.db.pool(), "l1_fragments", "idx", "integer").await;
        check_columns(&db.db.pool(), "l1_fragments", "total_bytes", "bigint").await;
        check_columns(&db.db.pool(), "l1_fragments", "unused_bytes", "bigint").await;
        check_columns(&db.db.pool(), "l1_fragments", "bundle_id", "integer").await;

        // Verify 'l1_transactions' has 'finalized_at' column
        check_columns(
            &db.db.pool(),
            "l1_transactions",
            "finalized_at",
            "timestamp with time zone",
        )
        .await;

        // Verify that l1_fragments and l1_transaction_fragments are empty after migration
        let count_l1_fragments = sqlx::query_scalar::<_, i64>(
            r#"
        SELECT COUNT(*) FROM l1_fragments
        "#,
        )
        .fetch_one(&db.db.pool())
        .await
        .unwrap();
        assert_eq!(
            count_l1_fragments, 0,
            "l1_fragments table is not empty after migration"
        );

        let count_l1_transaction_fragments = sqlx::query_scalar::<_, i64>(
            r#"
        SELECT COUNT(*) FROM l1_transaction_fragments
        "#,
        )
        .fetch_one(&db.db.pool())
        .await
        .unwrap();
        assert_eq!(
            count_l1_transaction_fragments, 0,
            "l1_transaction_fragments table is not empty after migration"
        );

        // Insert a default bundle to satisfy the foreign key constraint for future inserts
        let insert_default_bundle = r#"
        INSERT INTO bundles (start_height, end_height)
        VALUES ($1, $2)
        RETURNING id
    "#;
        let row = sqlx::query(insert_default_bundle)
            .bind(0i64)
            .bind(0i64)
            .fetch_one(&db.db.pool())
            .await
            .unwrap();
        let bundle_id: i32 = row.try_get("id").unwrap();
        assert_eq!(bundle_id, 1, "Default bundle ID is not 1");

        // Attempt to insert a fragment with empty data
        let insert_invalid_fragment = r#"
        INSERT INTO l1_fragments (idx, data, total_bytes, unused_bytes, bundle_id)
        VALUES ($1, $2, $3, $4, $5)
    "#;
        let result = sqlx::query(insert_invalid_fragment)
            .bind(1i32)
            .bind::<&[u8]>(&[]) // Empty data should fail due to check constraint
            .bind(10i64)
            .bind(5i64)
            .bind(1i32) // Valid bundle_id
            .execute(&db.db.pool())
            .await;

        assert!(
            result.is_err(),
            "Inserting empty data should fail due to check constraint"
        );

        // Insert a valid fragment
        let fragment_data_valid = vec![3u8; 15];
        let insert_valid_fragment = r#"
        INSERT INTO l1_fragments (idx, data, total_bytes, unused_bytes, bundle_id)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id
    "#;
        let row = sqlx::query(insert_valid_fragment)
            .bind(1i32)
            .bind(&fragment_data_valid)
            .bind(15i64)
            .bind(0i64)
            .bind(1i32)
            .fetch_one(&db.db.pool())
            .await
            .unwrap();

        let new_fragment_id: i32 = row.try_get("id").unwrap();
        assert!(new_fragment_id > 0, "Failed to insert a valid fragment");
    }

    #[tokio::test]
    async fn stress_test_update_costs() -> Result<()> {
        use services::{
            block_bundler::port::Storage, state_committer::port::Storage as CommitterStorage,
            state_listener::port::Storage as ListenerStorage,
        };

        let mut rng = rand::thread_rng();

        let storage = test_instance::PostgresProcess::shared()
            .await
            .expect("Failed to initialize PostgresProcess")
            .create_random_db()
            .await
            .expect("Failed to create random test database");

        let fragments_per_bundle = 1_000_000;
        let txs_per_fragment = 100;

        // insert the bundle and fragments
        let bundle_id = storage.next_bundle_id().await.unwrap();
        let end_height = rng.gen_range(1..5000);
        let range = 0..=end_height;

        // create fragments for the bundle
        let fragments = (0..fragments_per_bundle)
            .map(|_| Fragment {
                data: NonEmpty::from_vec(vec![rng.gen()]).unwrap(),
                unused_bytes: rng.gen_range(0..1000),
                total_bytes: rng.gen_range(1000..5000).try_into().unwrap(),
            })
            .collect::<Vec<_>>();
        let fragments = NonEmpty::from_vec(fragments).unwrap();

        storage
            .insert_bundle_and_fragments(bundle_id, range, fragments.clone())
            .await
            .unwrap();

        let fragment_ids = storage
            .oldest_nonfinalized_fragments(0, 2)
            .await
            .unwrap()
            .into_iter()
            .map(|f| f.id)
            .collect_nonempty()
            .unwrap();

        let mut tx_changes = vec![];
        let mut cost_updates = vec![];

        // for each fragment, create multiple transactions
        for _id in fragment_ids.iter() {
            for _ in 0..txs_per_fragment {
                let tx_hash = rng.gen::<[u8; 32]>();
                let tx = L1Tx {
                    hash: tx_hash,
                    nonce: rng.gen(),
                    ..Default::default()
                };

                storage
                    .record_pending_tx(tx.clone(), fragment_ids.clone(), Utc::now())
                    .await
                    .unwrap();

                // update transaction state to simulate finalized transactions
                let finalization_time = Utc::now();
                tx_changes.push((tx.hash, TransactionState::Finalized(finalization_time)));

                // cost updates
                let total_fee = rng.gen_range(1_000_000u128..10_000_000u128);
                let da_block_height = rng.gen_range(1_000_000u64..10_000_000u64);
                cost_updates.push(TransactionCostUpdate {
                    tx_hash,
                    total_fee,
                    da_block_height,
                });
            }
        }

        // update transaction states and costs
        let start_time = Instant::now();

        storage
            .update_tx_states_and_costs(tx_changes, vec![], cost_updates)
            .await
            .unwrap();

        let duration = start_time.elapsed();

        assert!(duration.as_secs() < 60);

        Ok(())
    }
}
