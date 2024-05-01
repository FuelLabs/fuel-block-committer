use std::marker::PhantomData;

use super::tables;
use crate::adapters::storage::{BlockSubmission, Error, Result};

pub(crate) struct Queries<Executor> {
    pub(crate) _data: PhantomData<Executor>,
}

impl<'a, Executor> Queries<Executor>
where
    Executor: sqlx::Executor<'a, Database = sqlx::Postgres> + Sync,
{
    pub(crate) async fn insert(executor: Executor, submission: BlockSubmission) -> Result<()> {
        let row = tables::EthFuelBlockSubmission::from(submission);
        sqlx::query!(
            "INSERT INTO eth_fuel_block_submission (fuel_block_hash, fuel_block_height, completed, submittal_height) VALUES ($1, $2, $3, $4)",
            row.fuel_block_hash,
            row.fuel_block_height,
            row.completed,
            row.submittal_height
        ).execute(executor).await?;
        Ok(())
    }

    pub(crate) async fn submission_w_latest_block(
        executor: Executor,
    ) -> Result<Option<BlockSubmission>> {
        sqlx::query_as!(
            tables::EthFuelBlockSubmission,
            "SELECT * FROM eth_fuel_block_submission ORDER BY fuel_block_height DESC LIMIT 1"
        )
        .fetch_optional(executor)
        .await?
        .map(BlockSubmission::try_from)
        .transpose()
    }

    pub(crate) async fn set_submission_completed(
        executor: Executor,
        fuel_block_hash: [u8; 32],
    ) -> Result<BlockSubmission> {
        let updated_row = sqlx::query_as!(
            tables::EthFuelBlockSubmission,
            "UPDATE eth_fuel_block_submission SET completed = true WHERE fuel_block_hash = $1 RETURNING *",
            fuel_block_hash.as_slice(),
        ).fetch_optional(executor).await?;

        if let Some(row) = updated_row {
            Ok(row.try_into()?)
        } else {
            let hash = hex::encode(fuel_block_hash);
            Err(Error::Database(format!("Cannot set submission to completed! Submission of block: `{hash}` not found in DB.")))
        }
    }
}
