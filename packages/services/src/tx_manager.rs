use async_trait::async_trait;
use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::{
    storage::Storage,
    types::{SubmissionTx, TransactionState, TransactionType},
};
use tracing::info;

use super::Runner;

pub struct TxManager<L1, Db> {
    l1_adapter: L1,
    storage: Db,
    num_blocks_to_finalize: u64,
    metrics: Metrics,
}

impl<L1, Db> TxManager<L1, Db> {
    pub fn new(l1_adapter: L1, storage: Db, num_blocks_to_finalize: u64) -> Self {
        Self {
            l1_adapter,
            storage,
            num_blocks_to_finalize,
            metrics: Metrics::default(),
        }
    }
}

impl<L1, Db> TxManager<L1, Db>
where
    L1: ports::l1::Api,
    Db: Storage,
{
    async fn check_pending_txs(&mut self, pending_txs: Vec<SubmissionTx>) -> crate::Result<()> {
        let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();

        for tx in pending_txs {
            let tx_hash = tx.hash;
            let Some(tx_response) = self.l1_adapter.get_transaction_response(tx_hash).await? else {
                continue; // not committed
            };

            if !tx_response.succeeded() {
                self.storage
                    .update_submission_tx_state(tx_hash, TransactionState::Failed)
                    .await?;

                info!(
                    "failed transaction {} of type {:?}",
                    hex::encode(tx_hash),
                    tx.tx_type
                );
                continue;
            }

            if current_block_number.saturating_sub(tx_response.block_number())
                < self.num_blocks_to_finalize
            {
                continue; // not finalized
            }

            self.storage
                .update_submission_tx_state(tx_hash, TransactionState::Finalized)
                .await?;

            info!(
                "finalized transaction {} of type {:?}",
                hex::encode(tx_hash),
                tx.tx_type
            );

            // Perform type-specific actions
            match tx.tx_type {
                TransactionType::ContractCommit => {
                    self.handle_contract_commit_finalized(&tx).await?
                }
                TransactionType::Blob => self.handle_blob_tx_finalized(&tx).await?,
            }
        }

        Ok(())
    }

    async fn handle_contract_commit_finalized(&self, tx: &SubmissionTx) -> crate::Result<()> {
        info!("Contract commit finalized: {}", hex::encode(tx.hash));
        Ok(())
    }

    async fn handle_blob_tx_finalized(&self, tx: &SubmissionTx) -> crate::Result<()> {
        info!("Blob transaction finalized: {}", hex::encode(tx.hash));
        Ok(())
    }
}

#[async_trait]
impl<L1, Db> Runner for TxManager<L1, Db>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
{
    async fn run(&mut self) -> crate::Result<()> {
        // poll pending txs
        // check if finalized, failed or stalled for too long
        // if stalled, resubmit with higher gas price
        // if failed, mark as failed -> maybe resubmit -> maybe notify
        // if finalized, mark as finalized
        // WHAT happens if tx is set to failed?
        // if tx hash cannot be found, maybe there was a network failure after saving
        // to db but before sending to L1. In this case, we should resubmit the tx
        // alternatively, we should record tx before sending to L1, then send, and potentially resend
        // the role of this service is to ensure that txs are either finalized or failed, if
        // it bumps fees and eventually we reach the "too_expenssive" threshold, we send a tx cancel

        let pending_txs = self.storage.get_pending_txs().await?;

        if pending_txs.is_empty() {
            return Ok(());
        }

        self.check_pending_txs(pending_txs).await?;

        Ok(())
    }
}

#[derive(Clone)]
struct Metrics {
    latest_committed_block: IntGauge,
}

impl<E, Db> RegistersMetrics for TxManager<E, Db> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![Box::new(self.metrics.latest_committed_block.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let latest_committed_block = IntGauge::with_opts(Opts::new(
            "latest_committed_block",
            "The height of the latest fuel block committed on Ethereum.",
        ))
        .expect("latest_committed_block metric to be correctly configured");

        Self {
            latest_committed_block,
        }
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate;
    use ports::types::{BlockSubmission, L1Height, TransactionResponse, U256};
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
            Ok(U256::ZERO)
        }

        async fn get_transaction_response(
            &self,
            tx_hash: [u8; 32],
        ) -> ports::l1::Result<Option<TransactionResponse>> {
            self.api.get_transaction_response(tx_hash).await
        }
    }

    fn given_l1_that_expects_get_transaction_receipt(
        tx_hash: [u8; 32],
        current_block_number: u32,
        block_number: u64,
    ) -> MockL1 {
        let mut l1 = MockL1::new();

        l1.api
            .expect_get_block_number()
            .return_once(move || Ok(current_block_number.into()));

        let transaction_response = TransactionResponse::new(block_number, true);
        l1.api
            .expect_get_transaction_response()
            .with(predicate::eq(tx_hash))
            .return_once(move |_| Ok(Some(transaction_response)));

        l1
    }

    fn given_l1_that_returns_failed_transaction(tx_hash: [u8; 32]) -> MockL1 {
        let mut l1 = MockL1::new();

        l1.api
            .expect_get_block_number()
            .return_once(move || Ok(0u32.into()));

        let transaction_response = TransactionResponse::new(0, false);

        l1.api
            .expect_get_transaction_response()
            .with(predicate::eq(tx_hash))
            .return_once(move |_| Ok(Some(transaction_response)));

        l1
    }

    fn given_block_submission() -> BlockSubmission {
        BlockSubmission {
            final_tx_id: None,
            block_hash: [1; 32],
            block_height: 1,
            submittal_height: 1.into(),
        }
    }

    #[tokio::test]
    async fn tx_manager_will_update_tx_state_if_finalized() -> crate::Result<()> {
        // given
        let tx_hash = [1; 32];
        let fragment_ids = vec![1, 2, 3];
        let block_submission = given_block_submission();

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;

        // record a blob and regular tx
        db.record_state_submission(tx_hash, fragment_ids).await?;
        db.record_block_submission(tx_hash, block_submission)
            .await?;

        let current_block_number = 34;
        let tx_block_number = 32;
        let l1_mock = given_l1_that_expects_get_transaction_receipt(
            tx_hash,
            current_block_number,
            tx_block_number,
        );

        let num_blocks_to_finalize = 1;
        let mut listener = TxManager::new(l1_mock, db.clone(), num_blocks_to_finalize);
        assert!(!db.get_pending_txs().await?.is_empty());

        // when
        listener.run().await.unwrap();

        // then
        assert!(db.get_pending_txs().await?.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn tx_manager_will_not_update_tx_state_if_not_finalized() -> crate::Result<()> {
        // given
        let tx_hash = [1; 32];
        let fragment_ids = vec![1, 2, 3];
        let block_submission = given_block_submission();

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;

        // record a blob and regular tx
        db.record_state_submission(tx_hash, fragment_ids).await?;
        db.record_block_submission(tx_hash, block_submission)
            .await?;

        let current_block_number = 34;
        let tx_block_number = 32;
        let l1_mock = given_l1_that_expects_get_transaction_receipt(
            tx_hash,
            current_block_number,
            tx_block_number,
        );

        let num_blocks_to_finalize = 4;
        let mut listener = TxManager::new(l1_mock, db.clone(), num_blocks_to_finalize);
        assert!(!db.get_pending_txs().await?.is_empty());

        // when
        listener.run().await.unwrap();

        // then
        assert!(!db.get_pending_txs().await?.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn state_listener_will_update_tx_state_if_failed() -> crate::Result<()> {
        // given
        let tx_hash = [1; 32];
        let fragment_ids = vec![1, 2, 3];
        let block_submission = given_block_submission();

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;

        // record a blob and regular tx
        db.record_state_submission(tx_hash, fragment_ids).await?;
        db.record_block_submission(tx_hash, block_submission)
            .await?;

        let l1_mock = given_l1_that_returns_failed_transaction(tx_hash);

        let num_blocks_to_finalize = 4;
        let mut listener = TxManager::new(l1_mock, db.clone(), num_blocks_to_finalize);
        assert!(!db.get_pending_txs().await?.is_empty());

        // when
        listener.run().await.unwrap();

        // then
        assert!(!db.get_pending_txs().await?.is_empty());

        Ok(())
    }
}
