use async_trait::async_trait;
use ports::storage::Storage;

use crate::{Result, Runner};

pub struct StateCommitter<L1, Db> {
    l1_adapter: L1,
    storage: Db,
}

impl<L1, Db> StateCommitter<L1, Db> {
    pub fn new(l1: L1, storage: Db) -> Self {
        Self {
            l1_adapter: l1,
            storage,
        }
    }
}

impl<L1, Db> StateCommitter<L1, Db>
where
    L1: ports::l1::Api,
    Db: Storage,
{
    async fn submit_state(&self) -> Result<()> {
        let fragments = self.storage.get_unsubmitted_fragments().await?;

        let data = fragments
            .into_iter()
            .flat_map(|fragment| fragment.raw_data)
            .collect::<Vec<_>>();

        let tx_hash = self.l1_adapter.submit_l2_state(data).await?;

        dbg!(tx_hash);
        self.storage.insert_pending_tx(tx_hash).await?;

        Ok(())
    }

    async fn is_tx_pending(&self) -> Result<bool> {
        let pending_txs = self.storage.get_pending_txs().await?;
        Ok(pending_txs.is_empty())
    }
}

#[async_trait]
impl<L1, Db> Runner for StateCommitter<L1, Db>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
{
    async fn run(&mut self) -> Result<()> {
        if self.is_tx_pending().await? {
            return Ok(());
        };

        self.submit_state().await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ports::{
        fuel::{FuelBlock, FuelBytes32, MockApi as FuelMockApi},
        types::{L1Height, U256},
    };
    use storage::PostgresProcess;
    use tai64::Tai64;

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
            Ok(0.into())
        }

        async fn balance(&self) -> ports::l1::Result<U256> {
            Ok(U256::zero())
        }
    }

    fn given_l1_that_expects_submission(block: ValidatedFuelBlock) -> MockL1 {
        let mut l1 = MockL1::new();

        l1.expect_submit_l2_state()
            .with(predicate::eq(block))
            .return_once(move |_| Ok([0; 32]));

        l1.contract
            .expect_submit()
            .with(predicate::eq(block))
            .return_once(move |_| Ok(()));

        l1.api
            .expect_get_block_number()
            .return_once(move || Ok(0u32.into()));

        l1
    }

    fn given_block() -> FuelBlock {
        let id = FuelBytes32::from([1u8; 32]);
        let header = ports::fuel::FuelHeader {
            id,
            da_height: 0,
            consensus_parameters_version: Default::default(),
            state_transition_bytecode_version: Default::default(),
            transactions_count: 1,
            message_receipt_count: 0,
            transactions_root: Default::default(),
            message_outbox_root: Default::default(),
            event_inbox_root: Default::default(),
            height: 1,
            prev_root: Default::default(),
            time: Tai64::now(),
            application_hash: Default::default(),
        };
        let block = FuelBlock {
            id,
            header,
            transactions: vec![],
            consensus: ports::fuel::FuelConsensus::Unknown,
            block_producer: Default::default(),
        };

        block
    }

    #[tokio::test]
    async fn test_submit_state() -> Result<()> {
        let fuel_mock = FuelMockApi::new();
        let block = given_block();

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        let committer = StateCommitter::new(db, fuel_mock);

        committer.submit_state(block).await.unwrap();

        Ok(())
    }
}
