use async_trait::async_trait;
use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::{
    clock::Clock,
    storage::Storage,
    types::{L1Tx, TransactionState},
};
use tracing::info;

use super::Runner;

pub struct StateListener<L1, Db, C> {
    l1_adapter: L1,
    storage: Db,
    num_blocks_to_finalize: u64,
    metrics: Metrics,
    clock: C,
}

impl<L1, Db, C> StateListener<L1, Db, C> {
    pub fn new(l1_adapter: L1, storage: Db, num_blocks_to_finalize: u64, clock: C) -> Self {
        Self {
            l1_adapter,
            storage,
            num_blocks_to_finalize,
            metrics: Metrics::default(),
            clock,
        }
    }
}

impl<L1, Db, C> StateListener<L1, Db, C>
where
    L1: ports::l1::Api,
    Db: Storage,
    C: Clock,
{
    async fn check_pending_txs(&mut self, pending_txs: Vec<L1Tx>) -> crate::Result<()> {
        println!("StateListener::check_pending_txs");
        let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();

        for tx in pending_txs {
            println!("StateListener::check_pending_txs tx: {:?}", tx);

            let tx_hash = tx.hash;
            let Some(tx_response) = self.l1_adapter.get_transaction_response(tx_hash).await? else {
                println!("StateListener::check_pending_txs tx_response is None");
                continue; // not committed
            };

            if !tx_response.succeeded() {
                self.storage
                    .update_tx_state(tx_hash, TransactionState::Failed)
                    .await?;

                info!("failed blob tx {}", hex::encode(tx_hash));
                continue;
            }

            if current_block_number.saturating_sub(tx_response.block_number())
                < self.num_blocks_to_finalize
            {
                continue; // not finalized
            }

            self.storage
                .update_tx_state(tx_hash, TransactionState::Finalized(self.clock.now()))
                .await?;

            info!("finalized blob tx {}", hex::encode(tx_hash));

            self.metrics
                .last_eth_block_w_blob
                .set(tx_response.block_number() as i64); // TODO: conversion
        }

        Ok(())
    }
}

#[async_trait]
impl<L1, Db, C> Runner for StateListener<L1, Db, C>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
    C: Clock + Send + Sync,
{
    async fn run(&mut self) -> crate::Result<()> {
        println!("StateListener::run");
        let pending_txs = self.storage.get_pending_txs().await?;
        println!("StateListener::run pending_txs: {:?}", pending_txs);

        if pending_txs.is_empty() {
            return Ok(());
        }

        self.check_pending_txs(pending_txs).await?;

        Ok(())
    }
}

#[derive(Clone)]
struct Metrics {
    last_eth_block_w_blob: IntGauge,
}

impl<L1, Db, C> RegistersMetrics for StateListener<L1, Db, C> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![Box::new(self.metrics.last_eth_block_w_blob.clone())]
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let last_eth_block_w_blob = IntGauge::with_opts(Opts::new(
            "last_eth_block_w_blob",
            "The height of the latest Ethereum block used for state submission.",
        ))
        .expect("last_eth_block_w_blob metric to be correctly configured");

        Self {
            last_eth_block_w_blob,
        }
    }
}

#[cfg(test)]
mod tests {
    // use clock::{SystemClock, TestClock};
    // use mockall::predicate;
    // use ports::types::{L1Height, StateFragment, StateSubmission, TransactionResponse, U256};
    // use storage::PostgresProcess;
    //
    // use super::*;
    //
    // struct MockL1 {
    //     api: ports::l1::MockApi,
    // }
    // impl MockL1 {
    //     fn new() -> Self {
    //         Self {
    //             api: ports::l1::MockApi::new(),
    //         }
    //     }
    // }
    //
    // #[async_trait::async_trait]
    // impl ports::l1::Api for MockL1 {
    //     async fn submit_l2_state(&self, _state_data: Vec<u8>) -> ports::l1::Result<[u8; 32]> {
    //         Ok([0; 32])
    //     }
    //
    //     async fn get_block_number(&self) -> ports::l1::Result<L1Height> {
    //         self.api.get_block_number().await
    //     }
    //
    //     async fn balance(&self) -> ports::l1::Result<U256> {
    //         Ok(U256::ZERO)
    //     }
    //
    //     async fn get_transaction_response(
    //         &self,
    //         tx_hash: [u8; 32],
    //     ) -> ports::l1::Result<Option<TransactionResponse>> {
    //         self.api.get_transaction_response(tx_hash).await
    //     }
    // }
    //
    // fn given_l1_that_expects_get_transaction_receipt(
    //     tx_hash: [u8; 32],
    //     current_block_number: u32,
    //     block_number: u64,
    // ) -> MockL1 {
    //     let mut l1 = MockL1::new();
    //
    //     l1.api
    //         .expect_get_block_number()
    //         .return_once(move || Ok(current_block_number.into()));
    //
    //     let transaction_response = TransactionResponse::new(block_number, true);
    //     l1.api
    //         .expect_get_transaction_response()
    //         .with(predicate::eq(tx_hash))
    //         .return_once(move |_| Ok(Some(transaction_response)));
    //
    //     l1
    // }
    //
    // fn given_l1_that_returns_failed_transaction(tx_hash: [u8; 32]) -> MockL1 {
    //     let mut l1 = MockL1::new();
    //
    //     l1.api
    //         .expect_get_block_number()
    //         .return_once(move || Ok(0u32.into()));
    //
    //     let transaction_response = TransactionResponse::new(0, false);
    //
    //     l1.api
    //         .expect_get_transaction_response()
    //         .with(predicate::eq(tx_hash))
    //         .return_once(move |_| Ok(Some(transaction_response)));
    //
    //     l1
    // }
    //
    // fn given_state() -> (StateSubmission, StateFragment, Vec<u32>) {
    //     let submission = StateSubmission {
    //         id: None,
    //         block_hash: [0u8; 32],
    //         block_height: 1,
    //     };
    //     let fragment_id = 1;
    //     let fragment = StateFragment {
    //         id: Some(fragment_id),
    //         submission_id: None,
    //         fragment_idx: 0,
    //         data: vec![1, 2, 3],
    //         created_at: ports::types::Utc::now(),
    //     };
    //     let fragment_ids = vec![fragment_id];
    //
    //     (submission, fragment, fragment_ids)
    // }
    //
    // #[tokio::test]
    // async fn state_listener_will_update_tx_state_if_finalized() -> crate::Result<()> {
    //     // given
    //     let (state, fragment, fragment_ids) = given_state();
    //     let tx_hash = [1; 32];
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_state_submission(state, vec![fragment]).await?;
    //     db.record_pending_tx(tx_hash, fragment_ids).await?;
    //
    //     let current_block_number = 34;
    //     let tx_block_number = 32;
    //     let l1_mock = given_l1_that_expects_get_transaction_receipt(
    //         tx_hash,
    //         current_block_number,
    //         tx_block_number,
    //     );
    //
    //     let num_blocks_to_finalize = 1;
    //     let test_clock = TestClock::default();
    //     let now = test_clock.now();
    //     let mut listener =
    //         StateListener::new(l1_mock, db.clone(), num_blocks_to_finalize, test_clock);
    //     assert!(db.has_pending_txs().await?);
    //
    //     // when
    //     listener.run().await.unwrap();
    //
    //     // then
    //     assert!(!db.has_pending_txs().await?);
    //     assert_eq!(db.last_time_a_fragment_was_finalized().await?.unwrap(), now);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn state_listener_will_not_update_tx_state_if_not_finalized() -> crate::Result<()> {
    //     // given
    //     let (state, fragment, fragment_ids) = given_state();
    //     let tx_hash = [1; 32];
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_state_submission(state, vec![fragment]).await?;
    //     db.record_pending_tx(tx_hash, fragment_ids).await?;
    //
    //     let current_block_number = 34;
    //     let tx_block_number = 32;
    //     let l1_mock = given_l1_that_expects_get_transaction_receipt(
    //         tx_hash,
    //         current_block_number,
    //         tx_block_number,
    //     );
    //
    //     let num_blocks_to_finalize = 4;
    //     let mut listener =
    //         StateListener::new(l1_mock, db.clone(), num_blocks_to_finalize, SystemClock);
    //     assert!(db.has_pending_txs().await?);
    //
    //     // when
    //     listener.run().await.unwrap();
    //
    //     // then
    //     assert!(db.has_pending_txs().await?);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn state_listener_will_update_tx_state_if_failed() -> crate::Result<()> {
    //     // given
    //     let (state, fragment, fragment_ids) = given_state();
    //     let tx_hash = [1; 32];
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_state_submission(state, vec![fragment]).await?;
    //     db.record_pending_tx(tx_hash, fragment_ids).await?;
    //
    //     let l1_mock = given_l1_that_returns_failed_transaction(tx_hash);
    //
    //     let num_blocks_to_finalize = 4;
    //     let mut listener =
    //         StateListener::new(l1_mock, db.clone(), num_blocks_to_finalize, SystemClock);
    //     assert!(db.has_pending_txs().await?);
    //
    //     // when
    //     listener.run().await.unwrap();
    //
    //     // then
    //     assert!(!db.has_pending_txs().await?);
    //
    //     Ok(())
    // }
}
