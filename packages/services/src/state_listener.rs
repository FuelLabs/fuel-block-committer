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
        let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();

        for tx in pending_txs {
            let tx_hash = tx.hash;
            let Some(tx_response) = self.l1_adapter.get_transaction_response(tx_hash).await? else {
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
                .set(i64::try_from(tx_response.block_number()).unwrap_or(i64::MAX))
        }

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
    use clock::TestClock;

    use super::*;
    use crate::test_utils::{
        self,
        mocks::{self, l1::TxStatus},
    };

    #[tokio::test]
    async fn state_listener_will_update_tx_state_if_finalized() -> crate::Result<()> {
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
        let mut listener =
            StateListener::new(l1_mock, setup.db(), num_blocks_to_finalize, test_clock);

        // when
        listener.run().await.unwrap();

        // then
        assert_eq!(setup.db().amount_of_pending_txs().await?, 0);
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
    async fn state_listener_will_not_update_tx_state_if_not_finalized() -> crate::Result<()> {
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
        );

        // when
        listener.run().await.unwrap();

        // then
        assert_ne!(setup.db().amount_of_pending_txs().await?, 0);
        assert!(setup
            .db()
            .last_time_a_fragment_was_finalized()
            .await?
            .is_none());

        Ok(())
    }

    #[tokio::test]
    async fn state_listener_will_update_tx_state_if_failed() -> crate::Result<()> {
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
        );

        // when
        listener.run().await.unwrap();

        // then
        assert_eq!(setup.db().amount_of_pending_txs().await?, 0);
        assert!(setup
            .db()
            .last_time_a_fragment_was_finalized()
            .await?
            .is_none());

        Ok(())
    }
}
