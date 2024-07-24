use async_trait::async_trait;
use ports::{
    storage::Storage,
    types::{SubmissionTx, TransactionReceipt, TransactionState},
};

use super::Runner;

pub struct StateListener<L1, Db> {
    l1_adapter: L1,
    storage: Db,
    num_blocks_to_finalize: u64,
}

impl<L1, Db> StateListener<L1, Db> {
    pub fn new(l1_adapter: L1, storage: Db, num_blocks_to_finalize: u64) -> Self {
        Self {
            l1_adapter,
            storage,
            num_blocks_to_finalize,
        }
    }
}

impl<L1, Db> StateListener<L1, Db>
where
    L1: ports::l1::Api,
    Db: Storage,
{
    async fn check_pending_txs(&mut self, pending_txs: Vec<SubmissionTx>) -> crate::Result<()> {
        let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();

        for tx in pending_txs {
            let Some(tx_receipt) = self.l1_adapter.get_transaction_receipt(tx.hash).await? else {
                continue; // not committed
            };

            // Status: either 1 (success) or 0 (failure).
            // Only present after activation of [EIP-658](https://eips.ethereum.org/EIPS/eip-658)
            if let Some(status) = tx_receipt.status {
                let status: u64 = status.try_into().map_err(|_| {
                    crate::Error::Other(
                        "could not convert tx receipt `status` to `u64`".to_string(),
                    )
                })?;

                if status == 0 {
                    self.storage
                        .update_submission_tx_state(tx.hash, TransactionState::Failed)
                        .await?;
                }

                continue;
            }

            let tx_block_number = Self::extract_block_number_from_receipt(tx_receipt)?;
            if current_block_number.saturating_sub(tx_block_number) < self.num_blocks_to_finalize {
                continue; // not finalized
            }

            self.storage
                .update_submission_tx_state(tx.hash, TransactionState::Finalized)
                .await?;
        }

        Ok(())
    }

    fn extract_block_number_from_receipt(receipt: TransactionReceipt) -> crate::Result<u64> {
        receipt
            .block_number
            .ok_or_else(|| {
                crate::Error::Other("transaction receipt does not contain block number".to_string())
            })?
            .try_into()
            .map_err(|_| {
                crate::Error::Other("could not convert `block_number` to `u64`".to_string())
            })
    }
}

#[async_trait]
impl<L1, Db> Runner for StateListener<L1, Db>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
{
    async fn run(&mut self) -> crate::Result<()> {
        let pending_txs = self.storage.get_pending_txs().await?;

        if pending_txs.is_empty() {
            return Ok(());
        }

        self.check_pending_txs(pending_txs).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate;
    use ports::types::{Fragment, L1Height, Submission, TransactionReceipt, U256};
    use storage::PostgresProcess;

    use super::*;

    struct MockL1 {
        api: ports::l1::MockApi,
    }
    impl MockL1 {
        fn new() -> Self {
            Self {
                api: ports::l1::MockApi::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ports::l1::Api for MockL1 {
        async fn submit_l2_state(&self, _state_data: Vec<u8>) -> ports::l1::Result<[u8; 32]> {
            Ok([0; 32])
        }

        async fn get_block_number(&self) -> ports::l1::Result<L1Height> {
            self.api.get_block_number().await
        }

        async fn balance(&self) -> ports::l1::Result<U256> {
            Ok(U256::zero())
        }

        async fn get_transaction_receipt(
            &self,
            tx_hash: [u8; 32],
        ) -> ports::l1::Result<Option<TransactionReceipt>> {
            self.api.get_transaction_receipt(tx_hash).await
        }
    }

    fn given_l1_that_expects_get_transaction_receipt(
        tx_hash: [u8; 32],
        current_block_number: u32,
        tx_block_number: u64,
    ) -> MockL1 {
        let mut l1 = MockL1::new();

        l1.api
            .expect_get_block_number()
            .return_once(move || Ok(current_block_number.into()));

        let transaction_receipt = TransactionReceipt {
            block_number: Some(tx_block_number.into()),
            ..Default::default()
        };

        l1.api
            .expect_get_transaction_receipt()
            .with(predicate::eq(tx_hash))
            .return_once(move |_| Ok(Some(transaction_receipt)));

        l1
    }

    fn given_l1_that_returns_failed_transaction(tx_hash: [u8; 32]) -> MockL1 {
        let mut l1 = MockL1::new();

        l1.api
            .expect_get_block_number()
            .return_once(move || Ok(0u32.into()));

        let transaction_receipt = TransactionReceipt {
            status: Some(0u64.into()),
            ..Default::default()
        };

        l1.api
            .expect_get_transaction_receipt()
            .with(predicate::eq(tx_hash))
            .return_once(move |_| Ok(Some(transaction_receipt)));

        l1
    }

    fn given_state() -> (Submission, Fragment, Vec<u32>) {
        let submission = Submission {
            id: None,
            block_hash: [0u8; 32],
            block_height: 1,
        };
        let fragment_id = 1;
        let fragment = Fragment {
            id: Some(fragment_id),
            submission_id: None,
            fragment_idx: 0,
            data: vec![1, 2, 3],
            created_at: ports::types::Utc::now(),
        };
        let fragment_ids = vec![fragment_id];

        (submission, fragment, fragment_ids)
    }

    #[tokio::test]
    async fn state_listener_will_update_tx_state_if_finalized() -> crate::Result<()> {
        // given
        let (state, fragment, fragment_ids) = given_state();
        let tx_hash = [1; 32];

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        db.insert_state_submission(state, vec![fragment]).await?;
        db.record_pending_tx(tx_hash, fragment_ids).await?;

        let current_block_number = 34;
        let tx_block_number = 32;
        let l1_mock = given_l1_that_expects_get_transaction_receipt(
            tx_hash,
            current_block_number,
            tx_block_number,
        );

        let num_blocks_to_finalize = 1;
        let mut listener = StateListener::new(l1_mock, db.clone(), num_blocks_to_finalize);
        assert!(db.has_pending_txs().await?);

        // when
        listener.run().await.unwrap();

        // then
        assert!(!db.has_pending_txs().await?);

        Ok(())
    }

    #[tokio::test]
    async fn state_listener_will_not_update_tx_state_if_not_finalized() -> crate::Result<()> {
        // given
        let (state, fragment, fragment_ids) = given_state();
        let tx_hash = [1; 32];

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        db.insert_state_submission(state, vec![fragment]).await?;
        db.record_pending_tx(tx_hash, fragment_ids).await?;

        let current_block_number = 34;
        let tx_block_number = 32;
        let l1_mock = given_l1_that_expects_get_transaction_receipt(
            tx_hash,
            current_block_number,
            tx_block_number,
        );

        let num_blocks_to_finalize = 4;
        let mut listener = StateListener::new(l1_mock, db.clone(), num_blocks_to_finalize);
        assert!(db.has_pending_txs().await?);

        // when
        listener.run().await.unwrap();

        // then
        assert!(db.has_pending_txs().await?);

        Ok(())
    }

    #[tokio::test]
    async fn state_listener_will_update_tx_state_if_failed() -> crate::Result<()> {
        // given
        let (state, fragment, fragment_ids) = given_state();
        let tx_hash = [1; 32];

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        db.insert_state_submission(state, vec![fragment]).await?;
        db.record_pending_tx(tx_hash, fragment_ids).await?;

        let l1_mock = given_l1_that_returns_failed_transaction(tx_hash);

        let num_blocks_to_finalize = 4;
        let mut listener = StateListener::new(l1_mock, db.clone(), num_blocks_to_finalize);
        assert!(db.has_pending_txs().await?);

        // when
        listener.run().await.unwrap();

        // then
        assert!(!db.has_pending_txs().await?);

        Ok(())
    }
}
