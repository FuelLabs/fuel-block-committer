use std::collections::HashSet;

use metrics::{
    prometheus::{core::Collector, IntGauge, Opts},
    RegistersMetrics,
};
use ports::{
    clock::Clock,
    storage::Storage,
    types::{L1Tx, TransactionCostUpdate, TransactionState},
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
    pub fn new(
        l1_adapter: L1,
        storage: Db,
        num_blocks_to_finalize: u64,
        clock: C,
        last_finalization_time_metric: IntGauge,
    ) -> Self {
        Self {
            l1_adapter,
            storage,
            num_blocks_to_finalize,
            metrics: Metrics::new(last_finalization_time_metric),
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
    async fn check_non_finalized_txs(&self, non_finalized_txs: Vec<L1Tx>) -> crate::Result<()> {
        let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();

        // we need to accumulate all the changes and then update the db atomically
        // to avoid race conditions with other services
        let mut skip_nonces = HashSet::new();
        let mut selective_change = vec![];
        let mut noncewide_changes = vec![];

        let mut cost_per_tx = vec![];

        for tx in non_finalized_txs {
            if skip_nonces.contains(&tx.nonce) {
                continue;
            }

            // get response if tx is included in a block
            let Some(tx_response) = self.l1_adapter.get_transaction_response(tx.hash).await? else {
                // not included in block - check what happened to the tx

                match (tx.state, self.l1_adapter.is_squeezed_out(tx.hash).await?) {
                    (TransactionState::Pending | TransactionState::IncludedInBlock, true) => {
                        // not in the mempool anymore set it to failed
                        selective_change.push((tx.hash, tx.nonce, TransactionState::Failed));

                        info!(
                            "blob tx {} not found in mempool. Setting to failed",
                            hex::encode(tx.hash)
                        );
                    }

                    (TransactionState::IncludedInBlock, false) => {
                        // if tx was in block and reorg happened now it is in the mempool - we need to set the tx to pending
                        selective_change.push((tx.hash, tx.nonce, TransactionState::Pending));

                        info!(
                            "blob tx {} returned to mempool. Setting to pending",
                            hex::encode(tx.hash)
                        );
                    }
                    _ => {}
                }

                continue;
            };

            skip_nonces.insert(tx.nonce);

            if !tx_response.succeeded() {
                // set tx to failed all txs with the same nonce to failed
                noncewide_changes.push((tx.hash, tx.nonce, TransactionState::Failed));

                info!("failed blob tx {}", hex::encode(tx.hash));
                continue;
            }

            if current_block_number.saturating_sub(tx_response.block_number())
                < self.num_blocks_to_finalize
            {
                // tx included in block but is not yet finalized
                if tx.state == TransactionState::Pending {
                    // set tx to included and all txs with the same nonce to failed
                    noncewide_changes.push((tx.hash, tx.nonce, TransactionState::IncludedInBlock));

                    info!(
                        "blob tx {} included in block {}",
                        hex::encode(tx.hash),
                        tx_response.block_number()
                    );
                }

                continue;
            }

            // st tx to finalized and all txs with the same nonce to failed
            let now = self.clock.now();
            noncewide_changes.push((tx.hash, tx.nonce, TransactionState::Finalized(now)));
            cost_per_tx.push(TransactionCostUpdate {
                tx_hash: tx.hash,
                total_fee: tx_response.total_fee(),
                da_block_height: tx_response.block_number(),
            });

            self.metrics.last_finalization_time.set(now.timestamp());

            info!("blob tx {} finalized", hex::encode(tx.hash));

            self.metrics
                .last_eth_block_w_blob
                .set(i64::try_from(tx_response.block_number()).unwrap_or(i64::MAX))
        }

        selective_change.retain(|(_, nonce, _)| !skip_nonces.contains(nonce));
        let selective_change: Vec<_> = selective_change
            .into_iter()
            .map(|(hash, _, state)| (hash, state))
            .collect();

        self.storage
            .update_tx_states_and_costs(selective_change, noncewide_changes, cost_per_tx)
            .await?;

        Ok(())
    }
}

impl<L1, Db, C> Runner for StateListener<L1, Db, C>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
    C: Clock + Send + Sync,
{
    async fn run(&mut self) -> crate::Result<()> {
        let non_finalized_txs = self.storage.get_non_finalized_txs().await?;

        if non_finalized_txs.is_empty() {
            return Ok(());
        }

        self.check_non_finalized_txs(non_finalized_txs).await?;

        Ok(())
    }
}

#[derive(Clone)]
struct Metrics {
    last_eth_block_w_blob: IntGauge,
    last_finalization_time: IntGauge,
}

impl<L1, Db, C> RegistersMetrics for StateListener<L1, Db, C> {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.last_eth_block_w_blob.clone()),
            Box::new(self.metrics.last_finalization_time.clone()),
        ]
    }
}

impl Metrics {
    fn new(last_finalization_time: IntGauge) -> Self {
        let last_eth_block_w_blob = IntGauge::with_opts(Opts::new(
            "last_eth_block_w_blob",
            "The height of the latest Ethereum block used for state submission.",
        ))
        .expect("last_eth_block_w_blob metric to be correctly configured");

        Self {
            last_eth_block_w_blob,
            last_finalization_time,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clock::TestClock;
    use mockall::predicate::eq;
    use ports::types::{L1Height, TransactionResponse, Utc};
    use test_case::test_case;

    use super::*;
    use crate::{
        test_utils::{
            self,
            mocks::{self, l1::TxStatus},
        },
        Result, StateCommitter,
    };

    #[tokio::test]
    async fn successful_finalized_tx() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let _ = setup.insert_fragments(0, 1).await;

        let tx_hash = [0; 32];
        setup.send_fragments(tx_hash).await;

        let num_blocks_to_finalize = 1u64;
        let current_height = 1;

        let tx_height = current_height - num_blocks_to_finalize;
        let l1_mock = mocks::l1::txs_finished(
            current_height as u32,
            tx_height as u32,
            [(tx_hash, TxStatus::Success)],
        );

        let test_clock = TestClock::default();
        let now = test_clock.now();
        let mut listener = StateListener::new(
            l1_mock,
            setup.db(),
            num_blocks_to_finalize,
            test_clock,
            IntGauge::new("test", "test").unwrap(),
        );

        // when
        listener.run().await.unwrap();

        // then
        assert!(!setup.db().has_pending_txs().await?);
        assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 0);
        assert_eq!(
            setup
                .db()
                .last_time_a_fragment_was_finalized()
                .await?
                .unwrap(),
            now
        );

        Ok(())
    }

    #[tokio::test]
    async fn successful_tx_in_block_not_finalized() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let _ = setup.insert_fragments(0, 1).await;

        let tx_hash = [0; 32];
        setup.send_fragments(tx_hash).await;

        let num_blocks_to_finalize = 5u64;
        let current_height = 5;

        let tx_height = current_height - 2;
        assert!(current_height - tx_height < num_blocks_to_finalize);

        let l1_mock = mocks::l1::txs_finished(
            current_height as u32,
            tx_height as u32,
            [(tx_hash, TxStatus::Success)],
        );

        let mut listener = StateListener::new(
            l1_mock,
            setup.db(),
            num_blocks_to_finalize,
            TestClock::default(),
            IntGauge::new("test", "test").unwrap(),
        );

        // when
        listener.run().await.unwrap();

        // then
        assert!(!setup.db().has_pending_txs().await?);
        assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 1);
        assert!(setup
            .db()
            .last_time_a_fragment_was_finalized()
            .await?
            .is_none());

        Ok(())
    }

    #[tokio::test]
    async fn from_pending_to_included_to_success_finalized_tx() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let _ = setup.insert_fragments(0, 1).await;

        let tx_hash = [0; 32];
        setup.send_fragments(tx_hash).await;
        assert!(setup.db().has_pending_txs().await?);

        let num_blocks_to_finalize = 5u64;
        let first_height = 5;
        let second_height = 8;

        let tx_height = first_height - 2;
        assert!(first_height - tx_height < num_blocks_to_finalize);

        let l1_mock = mocks::l1::txs_finished_multiple_heights(
            &[first_height as u32, second_height as u32],
            tx_height as u32,
            [(tx_hash, TxStatus::Success)],
        );

        let test_clock = TestClock::default();
        let mut listener = StateListener::new(
            l1_mock,
            setup.db(),
            num_blocks_to_finalize,
            test_clock.clone(),
            IntGauge::new("test", "test").unwrap(),
        );

        {
            // when first run - pending to included
            listener.run().await.unwrap();

            // then
            assert!(!setup.db().has_pending_txs().await?);
            assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 1);
            assert!(setup
                .db()
                .last_time_a_fragment_was_finalized()
                .await?
                .is_none());
        }
        {
            let now = test_clock.now();

            // when second run - included to finalized
            listener.run().await.unwrap();

            // then
            assert!(!setup.db().has_pending_txs().await?);
            assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 0);
            assert_eq!(
                setup
                    .db()
                    .last_time_a_fragment_was_finalized()
                    .await?
                    .unwrap(),
                now
            );
        }

        Ok(())
    }

    #[tokio::test]
    async fn reorg_threw_out_tx_from_block_into_pool() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let _ = setup.insert_fragments(0, 1).await;

        let tx_hash = [0; 32];
        setup.send_fragments(tx_hash).await;
        assert!(setup.db().has_pending_txs().await?);

        let num_blocks_to_finalize = 5u64;
        let first_height = 5;
        let second_height = 8;

        let tx_height = first_height - 2;
        assert!(first_height - tx_height < num_blocks_to_finalize);

        let l1_mock = mocks::l1::txs_reorg(
            &[first_height as u32, second_height as u32],
            tx_height as u32,
            (tx_hash, TxStatus::Success),
        );

        let test_clock = TestClock::default();
        let mut listener = StateListener::new(
            l1_mock,
            setup.db(),
            num_blocks_to_finalize,
            test_clock.clone(),
            IntGauge::new("test", "test").unwrap(),
        );

        {
            // when first run - pending to included
            listener.run().await.unwrap();

            // then
            assert!(!setup.db().has_pending_txs().await?);
            assert_eq!(setup.db().get_non_finalized_txs().await?.len(), 1);
            assert!(setup
                .db()
                .last_time_a_fragment_was_finalized()
                .await?
                .is_none());
        }
        {
            // when second run - included to pending
            listener.run().await.unwrap();

            // then
            assert!(setup.db().has_pending_txs().await?);
            assert!(setup
                .db()
                .last_time_a_fragment_was_finalized()
                .await?
                .is_none());
        }

        Ok(())
    }

    #[tokio::test]
    async fn reorg_threw_out_tx_from_block_into_pool_and_got_squeezed_out() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let _ = setup.insert_fragments(0, 1).await;

        let tx_hash = [0; 32];
        setup.send_fragments(tx_hash).await;

        let mut mock = ports::l1::MockApi::new();

        mock.expect_get_transaction_response()
            .once()
            .with(eq(tx_hash))
            .return_once(|_| {
                Box::pin(async { Ok(Some(TransactionResponse::new(1, true, 100, 10))) })
            });
        mock.expect_get_block_number()
            .returning(|| Box::pin(async { Ok(L1Height::from(1u32)) }));

        let mut listener = StateListener::new(
            mock,
            setup.db(),
            5,
            TestClock::default(),
            IntGauge::new("test", "test").unwrap(),
        );
        listener.run().await?;

        let mut l1 = ports::l1::MockApi::new();
        l1.expect_get_block_number()
            .returning(|| Box::pin(async { Ok(5.into()) }));
        l1.expect_get_transaction_response()
            .once()
            .with(eq(tx_hash))
            .return_once(|_| Box::pin(async { Ok(None) }));
        l1.expect_is_squeezed_out()
            .once()
            .with(eq(tx_hash))
            .return_once(|_| Box::pin(async { Ok(true) }));
        let mut listener = StateListener::new(
            l1,
            setup.db(),
            5,
            TestClock::default(),
            IntGauge::new("test", "test").unwrap(),
        );

        // when
        listener.run().await?;

        // then
        let db = setup.db();
        assert!(!db.has_nonfinalized_txs().await?);
        assert!(!db.has_pending_txs().await?);

        Ok(())
    }

    #[tokio::test]
    async fn tx_failed() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let _ = setup.insert_fragments(0, 1).await;

        let tx_hash = [0; 32];
        setup.send_fragments(tx_hash).await;

        let num_blocks_to_finalize = 5u64;
        let current_height = 5;

        let tx_height = current_height - 2;
        assert!(
            current_height - tx_height < num_blocks_to_finalize,
            "we should choose the tx height such that it's not finalized to showcase that we don't wait for finalization for failed txs"
        );

        let l1_mock = mocks::l1::txs_finished(
            current_height as u32,
            tx_height as u32,
            [(tx_hash, TxStatus::Failure)],
        );

        let mut listener = StateListener::new(
            l1_mock,
            setup.db(),
            num_blocks_to_finalize,
            TestClock::default(),
            IntGauge::new("test", "test").unwrap(),
        );

        // when
        listener.run().await.unwrap();

        // then
        assert!(!setup.db().has_pending_txs().await?);
        assert!(setup
            .db()
            .last_time_a_fragment_was_finalized()
            .await?
            .is_none());

        Ok(())
    }

    #[tokio::test]
    async fn fine_to_have_nothing_to_check() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        let mut listener = StateListener::new(
            ports::l1::MockApi::new(),
            setup.db(),
            5,
            TestClock::default(),
            IntGauge::new("test", "test").unwrap(),
        );

        // when
        let res = listener.run().await;

        // then
        assert!(res.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn a_pending_tx_got_squeezed_out() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;
        let _ = setup.insert_fragments(0, 1).await;

        let tx_hash = [0; 32];
        setup.send_fragments(tx_hash).await;
        let mut l1 = ports::l1::MockApi::new();
        l1.expect_get_block_number()
            .returning(|| Box::pin(async { Ok(5.into()) }));

        l1.expect_get_transaction_response()
            .with(eq(tx_hash))
            .once()
            .return_once(|_| Box::pin(async { Ok(None) }));

        l1.expect_is_squeezed_out()
            .with(eq(tx_hash))
            .once()
            .return_once(|_| Box::pin(async { Ok(true) }));

        let mut sut = StateListener::new(
            l1,
            setup.db(),
            5,
            TestClock::default(),
            IntGauge::new("test", "test").unwrap(),
        );

        // when
        sut.run().await?;

        // then
        assert!(!setup.db().has_pending_txs().await?);

        Ok(())
    }

    #[tokio::test]
    async fn block_inclusion_of_replacement_leaves_no_pending_txs() -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        // Insert multiple fragments with the same nonce
        let _ = setup.insert_fragments(0, 1).await;

        let start_time = Utc::now();
        let orig_tx_hash = [0; 32];
        let orig_tx = L1Tx {
            hash: orig_tx_hash,
            created_at: Some(start_time),
            ..Default::default()
        };

        let replacement_tx_time = start_time + Duration::from_secs(1);
        let replacement_tx_hash = [1; 32];
        let replacement_tx = L1Tx {
            hash: replacement_tx_hash,
            created_at: Some(replacement_tx_time),
            ..Default::default()
        };

        let clock = TestClock::default();
        let mut committer = StateCommitter::new(
            mocks::l1::expects_state_submissions(vec![(None, orig_tx), (None, replacement_tx)]),
            mocks::fuel::latest_height_is(0),
            setup.db(),
            crate::StateCommitterConfig {
                gas_bump_timeout: Duration::ZERO,
                ..Default::default()
            },
            clock.clone(),
        );

        // Orig tx
        committer.run().await?;

        // Replacement
        clock.set_time(replacement_tx_time);
        committer.run().await?;

        assert_eq!(setup.db().get_pending_txs().await.unwrap().len(), 2);

        let current_height = 10u64;
        let mut l1 = ports::l1::MockApi::new();
        l1.expect_get_block_number()
            .returning(move || Box::pin(async move { Ok(current_height.try_into().unwrap()) }));

        l1.expect_get_transaction_response()
            .with(eq(orig_tx_hash))
            .returning(|_| Box::pin(async { Ok(None) }));
        l1.expect_is_squeezed_out()
            .with(eq(orig_tx_hash))
            .returning(|_| Box::pin(async { Ok(true) }));
        l1.expect_get_transaction_response()
            .with(eq(replacement_tx_hash))
            .once()
            .return_once(move |_| {
                Box::pin(async move {
                    Ok(Some(TransactionResponse::new(
                        current_height,
                        true,
                        100,
                        10,
                    )))
                })
            });

        let mut listener = StateListener::new(
            l1,
            setup.db(),
            10,
            clock,
            IntGauge::new("test", "test").unwrap(),
        );

        // when
        listener.run().await?;

        // then
        let db = setup.db();
        assert!(!db.has_pending_txs().await?);
        assert!(db.has_nonfinalized_txs().await?);

        Ok(())
    }

    #[test_case(true ; "replacement tx succeeds")]
    #[test_case(false ; "replacement tx fails")]
    #[tokio::test]
    async fn finalized_replacement_tx_will_leave_no_pending_tx(
        replacement_tx_succeeded: bool,
    ) -> Result<()> {
        // given
        let setup = test_utils::Setup::init().await;

        // Insert multiple fragments with the same nonce
        let _ = setup.insert_fragments(0, 1).await;

        let start_time = Utc::now();
        let orig_tx_hash = [0; 32];
        let orig_tx = L1Tx {
            hash: orig_tx_hash,
            created_at: Some(start_time),
            ..Default::default()
        };

        let replacement_tx_time = start_time + Duration::from_secs(1);
        let replacement_tx_hash = [1; 32];
        let replacement_tx = L1Tx {
            hash: replacement_tx_hash,
            created_at: Some(replacement_tx_time),
            ..Default::default()
        };

        let clock = TestClock::default();
        let mut committer = StateCommitter::new(
            mocks::l1::expects_state_submissions(vec![(None, orig_tx), (None, replacement_tx)]),
            mocks::fuel::latest_height_is(0),
            setup.db(),
            crate::StateCommitterConfig {
                gas_bump_timeout: Duration::ZERO,
                ..Default::default()
            },
            clock.clone(),
        );

        // Orig tx
        committer.run().await?;

        // Replacement
        clock.set_time(replacement_tx_time);
        committer.run().await?;

        assert_eq!(setup.db().get_pending_txs().await.unwrap().len(), 2);

        let blocks_to_finalize = 1u64;
        let current_height = 10u64;
        let mut l1 = ports::l1::MockApi::new();
        l1.expect_get_block_number()
            .returning(move || Box::pin(async move { Ok(current_height.try_into().unwrap()) }));

        l1.expect_get_transaction_response()
            .with(eq(orig_tx_hash))
            .returning(|_| Box::pin(async { Ok(None) }));
        l1.expect_is_squeezed_out()
            .with(eq(orig_tx_hash))
            .returning(|_| Box::pin(async { Ok(true) }));
        l1.expect_get_transaction_response()
            .with(eq(replacement_tx_hash))
            .once()
            .return_once(move |_| {
                Box::pin(async move {
                    Ok(Some(TransactionResponse::new(
                        current_height - blocks_to_finalize,
                        replacement_tx_succeeded,
                        100,
                        10,
                    )))
                })
            });

        let mut listener = StateListener::new(
            l1,
            setup.db(),
            blocks_to_finalize,
            clock,
            IntGauge::new("test", "test").unwrap(),
        );

        // when
        listener.run().await?;

        // then
        let db = setup.db();
        assert!(!db.has_pending_txs().await?);
        assert!(!db.has_nonfinalized_txs().await?);

        Ok(())
    }
}
