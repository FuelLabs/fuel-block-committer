pub mod service {
    use std::num::NonZeroU32;

    use crate::{
        types::{fuel::FuelBlock, BlockSubmission, NonNegative, TransactionState},
        Error, Result,
    };
    use tracing::info;

    use crate::Runner;

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
        L1: crate::block_committer::port::l1::Contract + crate::block_committer::port::l1::Api,
        Db: crate::block_committer::port::Storage,
        Fuel: crate::block_committer::port::fuel::Api,
        Clock: crate::block_committer::port::Clock,
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
                let Some(tx_response) = self.l1_adapter.get_transaction_response(tx_hash).await?
                else {
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

                if !tx_response.confirmations(current_block_number) < self.num_blocks_to_finalize_tx
                {
                    continue; // not finalized
                }

                self.storage
                    .update_block_submission_tx(
                        tx_hash,
                        TransactionState::Finalized(self.clock.now()),
                    )
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
        L1: crate::block_committer::port::l1::Contract + crate::block_committer::port::l1::Api,
        Db: crate::block_committer::port::Storage,
        Fuel: crate::block_committer::port::fuel::Api,
        Clock: crate::block_committer::port::Clock + Send + Sync,
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
}

pub mod port {
    use crate::{
        types::{BlockSubmission, BlockSubmissionTx, DateTime, NonNegative, TransactionState, Utc},
        Result,
    };

    pub mod l1 {
        use std::num::NonZeroU32;

        use crate::{
            types::{BlockSubmissionTx, L1Height, TransactionResponse},
            Result,
        };

        #[allow(async_fn_in_trait)]
        #[trait_variant::make(Send)]
        #[cfg_attr(feature = "test-helpers", mockall::automock)]
        pub trait Contract: Send + Sync {
            async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;
            fn commit_interval(&self) -> NonZeroU32;
        }

        #[allow(async_fn_in_trait)]
        #[trait_variant::make(Send)]
        #[cfg_attr(feature = "test-helpers", mockall::automock)]
        pub trait Api {
            async fn get_block_number(&self) -> Result<L1Height>;
            async fn get_transaction_response(
                &self,
                tx_hash: [u8; 32],
            ) -> Result<Option<TransactionResponse>>;
        }
    }

    pub mod fuel {
        use crate::Result;
        pub use fuel_core_client::client::types::block::Block as FuelBlock;

        #[allow(async_fn_in_trait)]
        #[trait_variant::make(Send)]
        #[cfg_attr(feature = "test-helpers", mockall::automock)]
        pub trait Api: Send + Sync {
            async fn latest_block(&self) -> Result<FuelBlock>;
            async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>>;
        }
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Send + Sync {
        async fn record_block_submission(
            &self,
            submission_tx: BlockSubmissionTx,
            submission: BlockSubmission,
            created_at: DateTime<Utc>,
        ) -> Result<NonNegative<i32>>;
        async fn get_pending_block_submission_txs(
            &self,
            submission_id: NonNegative<i32>,
        ) -> Result<Vec<BlockSubmissionTx>>;
        async fn update_block_submission_tx(
            &self,
            hash: [u8; 32],
            state: TransactionState,
        ) -> Result<BlockSubmission>;
        async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    }

    pub trait Clock {
        fn now(&self) -> DateTime<Utc>;
    }
}
