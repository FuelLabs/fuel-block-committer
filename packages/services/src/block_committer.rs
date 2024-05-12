use async_trait::async_trait;
use ports::{
    storage::Storage,
    types::{BlockSubmission, ValidatedFuelBlock},
};
use tokio::sync::mpsc::Receiver;
use tracing::{error, info};

use super::Runner;
use crate::Result;

pub struct BlockCommitter<C, Db> {
    rx_block: Receiver<ValidatedFuelBlock>,
    l1: C,
    storage: Db,
}

impl<L1, Db> BlockCommitter<L1, Db> {
    pub fn new(rx_block: Receiver<ValidatedFuelBlock>, l1: L1, storage: Db) -> Self {
        Self {
            rx_block,
            l1,
            storage,
        }
    }

    async fn next_fuel_block_for_committal(&mut self) -> Option<ValidatedFuelBlock> {
        self.rx_block.recv().await
    }
}

impl<A, Db> BlockCommitter<A, Db>
where
    A: ports::l1::Contract + ports::l1::Api,
    Db: Storage,
{
    async fn submit_block(&self, fuel_block: ValidatedFuelBlock) -> Result<()> {
        let submittal_height = self.l1.get_block_number().await?;

        let submission = BlockSubmission {
            block: fuel_block,
            submittal_height,
            completed: false,
        };

        self.storage.insert(submission).await?;

        // if we have a network failure the DB entry will be left at completed:false.
        self.l1.submit(fuel_block).await?;

        Ok(())
    }
}

#[async_trait]
impl<A, Db> Runner for BlockCommitter<A, Db>
where
    A: ports::l1::Contract + ports::l1::Api,
    Db: Storage,
{
    async fn run(&mut self) -> Result<()> {
        while let Some(fuel_block) = self.next_fuel_block_for_committal().await {
            if let Err(error) = self.submit_block(fuel_block).await {
                error!("{error}");
            } else {
                info!("submitted {fuel_block:?}!");
            }
        }

        info!("Block Committer stopped");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mockall::predicate;
    use ports::{
        l1::{Contract, EventStreamer, MockApi, MockContract},
        types::{L1Height, U256},
    };
    use rand::Rng;
    use storage::PostgresProcess;

    use super::*;

    struct MockL1 {
        api: MockApi,
        contract: MockContract,
    }

    #[async_trait::async_trait]
    impl Contract for MockL1 {
        async fn submit(&self, block: ValidatedFuelBlock) -> ports::l1::Result<()> {
            self.contract.submit(block).await
        }
        fn event_streamer(&self, height: L1Height) -> Box<dyn EventStreamer + Send + Sync> {
            self.contract.event_streamer(height)
        }
    }

    #[cfg_attr(feature = "test-helpers", mockall::automock)]
    #[async_trait::async_trait]
    impl ports::l1::Api for MockL1 {
        async fn get_block_number(&self) -> ports::l1::Result<L1Height> {
            self.api.get_block_number().await
        }
        async fn balance(&self) -> ports::l1::Result<U256> {
            self.api.balance().await
        }
    }

    #[tokio::test]
    async fn block_committer_will_submit_and_write_block() {
        // given
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let block: ValidatedFuelBlock = rand::thread_rng().gen();
        let expected_height = block.height();
        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await.unwrap();

        let mock_l1 = given_l1_that_expects_submission(block);
        tx.try_send(block).unwrap();

        // when
        spawn_committer_and_run_until_timeout(rx, mock_l1, db.clone()).await;

        // then
        let last_submission = db.submission_w_latest_block().await.unwrap().unwrap();
        assert_eq!(expected_height, last_submission.block.height());
    }

    fn given_l1_that_expects_submission(block: ValidatedFuelBlock) -> MockL1 {
        let mut l1 = MockL1 {
            api: MockApi::new(),
            contract: MockContract::new(),
        };
        l1.contract
            .expect_submit()
            .with(predicate::eq(block))
            .return_once(move |_| Ok(()));

        l1.api
            .expect_get_block_number()
            .return_once(move || Ok(0u32.into()));

        l1
    }

    async fn spawn_committer_and_run_until_timeout<Db: Storage>(
        rx: Receiver<ValidatedFuelBlock>,
        mock_l1: MockL1,
        storage: Db,
    ) {
        let _ = tokio::time::timeout(Duration::from_millis(250), async move {
            let mut block_committer = BlockCommitter::new(rx, mock_l1, storage);
            block_committer
                .run()
                .await
                .expect("Errors are handled inside of run");
        })
        .await;
    }
}
