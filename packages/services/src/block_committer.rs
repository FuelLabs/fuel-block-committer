use std::num::NonZeroU32;

use crate::{
    ports::{fuel::FuelBlock, storage::Storage},
    types::{BlockSubmission, NonNegative, TransactionState},
    Error, Result,
};
use tracing::info;

use super::Runner;

pub struct BlockCommitter<L1, Db, Fuel, Clock> {
    l1_adapter: L1,
    fuel_adapter: Fuel,
    storage: Db,
    clock: Clock,
    commit_interval: NonZeroU32,
    num_blocks_to_finalize_tx: u64,
}

#[derive(Debug)]
enum Action {
    UpdateTx {
        submission_id: NonNegative<i32>,
        block_height: u32,
    },
    Post,
    DoNothing,
}

impl<L1, Db, Fuel, Clock> BlockCommitter<L1, Db, Fuel, Clock> {
    pub fn new(
        l1: L1,
        storage: Db,
        fuel_adapter: Fuel,
        clock: Clock,
        commit_interval: NonZeroU32,
        num_blocks_to_finalize_tx: u64,
    ) -> Self {
        Self {
            l1_adapter: l1,
            storage,
            fuel_adapter,
            clock,
            commit_interval,
            num_blocks_to_finalize_tx,
        }
    }
}

impl<L1, Db, Fuel, Clock> BlockCommitter<L1, Db, Fuel, Clock>
where
    L1: crate::ports::l1::Contract + crate::ports::l1::Api,
    Db: Storage,
    Fuel: crate::ports::fuel::Api,
    Clock: crate::ports::clock::Clock,
{
    async fn submit_block(&self, fuel_block: FuelBlock) -> Result<()> {
        let submission = BlockSubmission::new(*fuel_block.id, fuel_block.header.height);

        let mut tx = self
            .l1_adapter
            .submit(*fuel_block.id, fuel_block.header.height)
            .await?;
        tx.submission_id = submission.id;
        self.storage
            .record_block_submission(tx, submission, self.clock.now())
            .await?;

        info!("submitted {fuel_block:?}!");

        Ok(())
    }

    fn current_epoch_block_height(&self, current_block_height: u32) -> u32 {
        current_block_height - (current_block_height % self.commit_interval)
    }

    async fn fetch_block(&self, height: u32) -> Result<FuelBlock> {
        let fuel_block = self
            .fuel_adapter
            .block_at_height(height)
            .await?
            .ok_or_else(|| {
                Error::Other(format!(
                    "Fuel node could not provide block at height: {height}"
                ))
            })?;

        Ok(fuel_block)
    }

    fn decide_action(
        &self,
        latest_submission: Option<&BlockSubmission>,
        current_epoch_block_height: u32,
    ) -> Action {
        let is_stale = latest_submission
            .map(|s| s.block_height < current_epoch_block_height)
            .unwrap_or(true);

        // if we reached the next epoch since our last submission, we should post the latest block
        if is_stale {
            return Action::Post;
        }

        match latest_submission {
            Some(submission) if !submission.completed => Action::UpdateTx {
                submission_id: submission.id.expect("submission to have id"),
                block_height: submission.block_height,
            },
            _ => Action::DoNothing,
        }
    }

    async fn update_transactions(
        &self,
        submission_id: NonNegative<i32>,
        block_height: u32,
    ) -> Result<()> {
        let transactions = self
            .storage
            .get_pending_block_submission_txs(submission_id)
            .await?;
        let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();

        for tx in transactions {
            let tx_hash = tx.hash;
            let Some(tx_response) = self.l1_adapter.get_transaction_response(tx_hash).await? else {
                continue; // not included
            };

            if !tx_response.succeeded() {
                self.storage
                    .update_block_submission_tx(tx_hash, TransactionState::Failed)
                    .await?;

                info!(
                    "failed submission for block: {block_height} with tx: {}",
                    hex::encode(tx_hash)
                );
                continue;
            }

            if !tx_response.confirmations(current_block_number) < self.num_blocks_to_finalize_tx {
                continue; // not finalized
            }

            self.storage
                .update_block_submission_tx(tx_hash, TransactionState::Finalized(self.clock.now()))
                .await?;

            info!(
                "finalized submission for block: {block_height} with tx: {}",
                hex::encode(tx_hash)
            );
        }

        Ok(())
    }
}

impl<L1, Db, Fuel, Clock> Runner for BlockCommitter<L1, Db, Fuel, Clock>
where
    L1: crate::ports::l1::Contract + crate::ports::l1::Api,
    Db: Storage,
    Fuel: crate::ports::fuel::Api,
    Clock: crate::ports::clock::Clock + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        let latest_submission = self.storage.submission_w_latest_block().await?;

        let current_block = self.fuel_adapter.latest_block().await?;
        let current_epoch_block_height =
            self.current_epoch_block_height(current_block.header.height);

        let action = self.decide_action(latest_submission.as_ref(), current_epoch_block_height);
        match action {
            Action::DoNothing => {}
            Action::Post => {
                let block = if current_block.header.height == current_epoch_block_height {
                    current_block
                } else {
                    self.fetch_block(current_epoch_block_height).await?
                };

                self.submit_block(block).await?;
            }
            Action::UpdateTx {
                submission_id,
                block_height,
            } => {
                self.update_transactions(submission_id, block_height)
                    .await?
            }
        }

        Ok(())
    }
}
