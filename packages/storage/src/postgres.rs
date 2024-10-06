use std::ops::RangeInclusive;

use itertools::Itertools;
use metrics::{prometheus::IntGauge, RegistersMetrics};
use ports::{
    storage::SequentialFuelBlocks,
    types::{
        BlockSubmission, DateTime, Fragment, NonEmpty, NonNegative, TransactionState,
        TryCollectNonEmpty, Utc,
    },
};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    QueryBuilder,
};

use super::error::{Error, Result};
use crate::mappings::tables::{self, L1TxState};

#[derive(Debug, Clone)]
struct Metrics {
    height_of_latest_commitment: IntGauge,
    seconds_since_last_finalized_fragment: IntGauge,
    lowest_unbundled_height: IntGauge,
}

impl Default for Metrics {
    fn default() -> Self {
        let height_of_latest_commitment = IntGauge::new(
            "height_of_latest_commitment",
            "The height of the latest commitment",
        )
        .expect("height_of_latest_commitment gauge to be correctly configured");

        let seconds_since_last_finalized_fragment = IntGauge::new(
            "seconds_since_last_finalized_fragment",
            "The number of seconds since the last finalized fragment",
        )
        .expect("seconds_since_last_finalized_fragment gauge to be correctly configured");

        let lowest_unbundled_height = IntGauge::new(
            "lowest_unbundled_height",
            "The height of the lowest block unbundled block",
        )
        .expect("lowest_unbundled_height gauge to be correctly configured");

        Self {
            height_of_latest_commitment,
            seconds_since_last_finalized_fragment,
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
        vec![Box::new(self.metrics.height_of_latest_commitment.clone())]
    }
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

    pub async fn migrate(&self) -> ports::storage::Result<()> {
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

    pub(crate) async fn _oldest_nonfinalized_fragments(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Vec<ports::storage::BundleFragment>> {
        let limit: i64 = limit.try_into().unwrap_or(i64::MAX);
        let fragments = sqlx::query_as!(
            tables::BundleFragment,
            r#"
            SELECT f.*
            FROM l1_fragments f
            LEFT JOIN l1_transaction_fragments tf ON tf.fragment_id = f.id
            LEFT JOIN l1_transactions t ON t.id = tf.transaction_id
            JOIN bundles b ON b.id = f.bundle_id
            WHERE (t.id IS NULL OR t.state = $1) 
              AND b.end_height >= $2 -- Exclude bundles ending before starting_height
            ORDER BY b.start_height ASC, f.idx ASC
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

    pub(crate) async fn _insert_blocks(
        &self,
        blocks: NonEmpty<ports::storage::FuelBlock>,
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
            MAX(l1_transactions.finalized_at) AS last_fragment_time
        FROM
            l1_transaction_fragments
        JOIN
            l1_transactions ON l1_transactions.id = l1_transaction_fragments.transaction_id
        WHERE
            l1_transactions.state = $1;
        "#,
            i16::from(L1TxState::Finalized)
        )
        .fetch_optional(&self.connection_pool)
        .await?
        .and_then(|response| response.last_fragment_time);

        if let Some(last_fragment_time) = response {
            let now = Utc::now();
            let seconds_since_last_finalized_fragment =
                now.signed_duration_since(last_fragment_time).num_seconds();
            self.metrics
                .seconds_since_last_finalized_fragment
                .set(seconds_since_last_finalized_fragment);
        }

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

        let sequential_blocks = response
            .into_iter()
            .map(ports::storage::FuelBlock::try_from)
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
        tx_hash: [u8; 32],
        fragment_ids: NonEmpty<NonNegative<i32>>,
    ) -> Result<()> {
        let mut tx = self.connection_pool.begin().await?;

        let tx_id = sqlx::query!(
            "INSERT INTO l1_transactions (hash, state) VALUES ($1, $2) RETURNING id",
            tx_hash.as_slice(),
            i16::from(L1TxState::Pending)
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
            "SELECT EXISTS (SELECT 1 FROM l1_transactions WHERE state = $1) AS has_pending_transactions;",
            i16::from(L1TxState::Pending)
        )
        .fetch_one(&self.connection_pool)
        .await?
        .has_pending_transactions.unwrap_or(false))
    }

    pub(crate) async fn _get_pending_txs(&self) -> Result<Vec<ports::types::L1Tx>> {
        sqlx::query_as!(
            tables::L1Tx,
            "SELECT * FROM l1_transactions WHERE state = $1",
            i16::from(L1TxState::Pending)
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
        let finalized_at = match &state {
            TransactionState::Finalized(date_time) => Some(*date_time),
            _ => None,
        };
        let state = i16::from(L1TxState::from(&state));

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
        fragments: NonEmpty<Fragment>,
    ) -> Result<()> {
        let mut tx = self.connection_pool.begin().await?;

        let start = *block_range.start();
        let end = *block_range.end();

        // Insert a new bundle and retrieve its ID
        let bundle_id = sqlx::query!(
            "INSERT INTO bundles(start_height, end_height) VALUES ($1, $2) RETURNING id",
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

    use sqlx::{Executor, PgPool, Row};

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
}
