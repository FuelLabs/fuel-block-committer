use std::time::Duration;

use async_trait::async_trait;
use ports::{
    clock::Clock,
    storage::Storage,
    types::{DateTime, Utc},
};
use tracing::{info, warn};

use crate::{Result, Runner};

pub struct StateCommitter<L1, Db, Clock> {
    l1_adapter: L1,
    storage: Db,
    clock: Clock,
    bundle_config: BundleGenerationConfig,
    component_created_at: DateTime<Utc>,
}

pub struct BundleGenerationConfig {
    pub num_blocks: usize,
    pub accumulation_timeout: Duration,
}

impl<L1, Db, C: Clock> StateCommitter<L1, Db, C> {
    pub fn new(l1: L1, storage: Db, clock: C, bundle_config: BundleGenerationConfig) -> Self {
        let now = clock.now();
        Self {
            l1_adapter: l1,
            storage,
            clock,
            bundle_config,
            component_created_at: now,
        }
    }
}

impl<L1, Db, C> StateCommitter<L1, Db, C>
where
    L1: ports::l1::Api,
    Db: Storage,
    C: Clock,
{
    async fn fetch_fragments(&self) -> Result<(Vec<u32>, Vec<u8>)> {
        let fragments = self.storage.stream_unfinalized_segment_data().await?;

        let num_fragments = fragments.len();
        let mut fragment_ids = Vec::with_capacity(num_fragments);
        let mut data = Vec::with_capacity(num_fragments);
        for fragment in fragments {
            fragment_ids.push(fragment.id.expect("fragments from DB must have `id`"));
            data.extend(fragment.data);
        }

        Ok((fragment_ids, data))
    }

    async fn submit_state(&self) -> Result<()> {
        Ok(())
        // // TODO: segfault, what about encoding overhead?
        // let (fragment_ids, data) = self.fetch_fragments().await?;
        //
        // // TODO: segfault what about when the fragments don't add up cleanly to max_total_size
        // if data.len() < max_total_size {
        //     let fragment_count = fragment_ids.len();
        //     let data_size = data.len();
        //     let remaining_space = max_total_size.saturating_sub(data_size);
        //
        //     let last_finalization = self
        //         .storage
        //         .last_time_a_fragment_was_finalized()
        //         .await?
        //         .unwrap_or_else(|| {
        //             info!("No fragment has been finalized yet, accumulation timeout will be calculated from the time the committer was started ({})", self.component_created_at);
        //             self.component_created_at
        //         });
        //
        //     let now = self.clock.now();
        //     let time_delta = now - last_finalization;
        //
        //     let duration = time_delta
        //         .to_std()
        //         .unwrap_or_else(|_| {
        //             warn!("possible time skew, last fragment finalization happened at {last_finalization}, with the current clock time at: {now} making for a difference of: {time_delta}");
        //             // we act as if the finalization happened now
        //             Duration::ZERO
        //         });
        //
        //     if duration < self.accumulation_timeout {
        //         info!("Found {fragment_count} fragment(s) with total size of {data_size}B. Waiting for additional fragments to use up more of the remaining {remaining_space}B.");
        //         return Ok(());
        //     } else {
        //         info!("Found {fragment_count} fragment(s) with total size of {data_size}B. Accumulation timeout has expired, proceeding to submit.")
        //     }
        // }
        //
        // if fragment_ids.is_empty() {
        //     return Ok(());
        // }
        //
        // let tx_hash = self.l1_adapter.submit_l2_state(data).await?;
        // self.storage
        //     .record_pending_tx(tx_hash, fragment_ids)
        //     .await?;
        //
        // info!("submitted blob tx {}", hex::encode(tx_hash));
        //
        // Ok(())
    }

    async fn is_tx_pending(&self) -> Result<bool> {
        self.storage.has_pending_txs().await.map_err(|e| e.into())
    }
}

#[async_trait]
impl<L1, Db, C> Runner for StateCommitter<L1, Db, C>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage,
    C: Send + Sync + Clock,
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
    #[allow(dead_code)]
    fn setup_logger() {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_level(true)
            .with_line_number(true)
            .json()
            .init();
    }

    use clock::TestClock;
    use mockall::predicate;
    use ports::types::{
        L1Height, StateFragment, StateSubmission, TransactionResponse, TransactionState, U256,
    };
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
        async fn submit_l2_state(&self, state_data: Vec<u8>) -> ports::l1::Result<[u8; 32]> {
            self.api.submit_l2_state(state_data).await
        }

        async fn get_block_number(&self) -> ports::l1::Result<L1Height> {
            Ok(0.into())
        }

        async fn balance(&self) -> ports::l1::Result<U256> {
            Ok(U256::ZERO)
        }

        async fn get_transaction_response(
            &self,
            _tx_hash: [u8; 32],
        ) -> ports::l1::Result<Option<TransactionResponse>> {
            Ok(None)
        }
    }

    fn given_l1_that_expects_submission(data: Vec<u8>) -> MockL1 {
        let mut l1 = MockL1::new();

        l1.api
            .expect_submit_l2_state()
            .with(predicate::eq(data))
            .return_once(move |_| Ok([1u8; 32]));

        l1
    }

    #[tokio::test]
    async fn will_bundle_and_fragment_if_none_available() -> Result<()> {
        //given
        let l1_mock = MockL1::new();
        let blocks = vec![];

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        let config = BundleGenerationConfig {
            num_blocks: 2,
            accumulation_timeout: Duration::from_secs(1),
        };

        let mut committer = StateCommitter::new(l1_mock, db.clone(), TestClock::default(), config);

        // when
        committer.run().await.unwrap();

        // then
        assert!(!db.has_pending_txs().await?);

        Ok(())
    }

    // #[tokio::test]
    // async fn will_wait_for_more_data() -> Result<()> {
    //     // given
    //     let (block_1_state, block_1_state_fragment) = (
    //         StateSubmission {
    //             id: None,
    //             block_hash: [0u8; 32],
    //             block_height: 1,
    //         },
    //         StateFragment {
    //             id: None,
    //             submission_id: None,
    //             fragment_idx: 0,
    //             data: vec![0; 127_000],
    //             created_at: ports::types::Utc::now(),
    //         },
    //     );
    //     let l1_mock = MockL1::new();
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_state_submission(block_1_state, vec![block_1_state_fragment])
    //         .await?;
    //
    //     let mut committer = StateCommitter::new(
    //         l1_mock,
    //         db.clone(),
    //         TestClock::default(),
    //         Duration::from_secs(1),
    //     );
    //
    //     // when
    //     committer.run().await.unwrap();
    //
    //     // then
    //     assert!(!db.has_pending_txs().await?);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn triggers_when_enough_data_is_made_available() -> Result<()> {
    //     // given
    //     let max_data = 6 * 128 * 1024;
    //     let (block_1_state, block_1_state_fragment) = (
    //         StateSubmission {
    //             id: None,
    //             block_hash: [0u8; 32],
    //             block_height: 1,
    //         },
    //         StateFragment {
    //             id: None,
    //             submission_id: None,
    //             fragment_idx: 0,
    //             data: vec![1; max_data - 1000],
    //             created_at: ports::types::Utc::now(),
    //         },
    //     );
    //
    //     let (block_2_state, block_2_state_fragment) = (
    //         StateSubmission {
    //             id: None,
    //             block_hash: [1u8; 32],
    //             block_height: 2,
    //         },
    //         StateFragment {
    //             id: None,
    //             submission_id: None,
    //             fragment_idx: 0,
    //             data: vec![1; 1000],
    //             created_at: ports::types::Utc::now(),
    //         },
    //     );
    //     let l1_mock = given_l1_that_expects_submission(
    //         [
    //             block_1_state_fragment.data.clone(),
    //             block_2_state_fragment.data.clone(),
    //         ]
    //         .concat(),
    //     );
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_state_submission(block_1_state, vec![block_1_state_fragment])
    //         .await?;
    //
    //     let mut committer = StateCommitter::new(
    //         l1_mock,
    //         db.clone(),
    //         TestClock::default(),
    //         Duration::from_secs(1),
    //     );
    //     committer.run().await?;
    //     assert!(!db.has_pending_txs().await?);
    //     assert!(db.get_pending_txs().await?.is_empty());
    //
    //     db.insert_state_submission(block_2_state, vec![block_2_state_fragment])
    //         .await?;
    //     tokio::time::sleep(Duration::from_millis(2000)).await;
    //
    //     // when
    //     committer.run().await?;
    //
    //     // then
    //     assert!(!db.get_pending_txs().await?.is_empty());
    //     assert!(db.has_pending_txs().await?);
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn will_trigger_on_accumulation_timeout() -> Result<()> {
    //     // given
    //     let (block_1_state, block_1_submitted_fragment, block_1_unsubmitted_state_fragment) = (
    //         StateSubmission {
    //             id: None,
    //             block_hash: [0u8; 32],
    //             block_height: 1,
    //         },
    //         StateFragment {
    //             id: None,
    //             submission_id: None,
    //             fragment_idx: 0,
    //             data: vec![0; 100],
    //             created_at: ports::types::Utc::now(),
    //         },
    //         StateFragment {
    //             id: None,
    //             submission_id: None,
    //             fragment_idx: 0,
    //             data: vec![0; 127_000],
    //             created_at: ports::types::Utc::now(),
    //         },
    //     );
    //
    //     let l1_mock =
    //         given_l1_that_expects_submission(block_1_unsubmitted_state_fragment.data.clone());
    //
    //     let process = PostgresProcess::shared().await.unwrap();
    //     let db = process.create_random_db().await?;
    //     db.insert_state_submission(
    //         block_1_state,
    //         vec![
    //             block_1_submitted_fragment,
    //             block_1_unsubmitted_state_fragment,
    //         ],
    //     )
    //     .await?;
    //
    //     let clock = TestClock::default();
    //
    //     db.record_pending_tx([0; 32], vec![1]).await?;
    //     db.update_submission_tx_state([0; 32], TransactionState::Finalized(clock.now()))
    //         .await?;
    //
    //     let accumulation_timeout = Duration::from_secs(1);
    //     let mut committer =
    //         StateCommitter::new(l1_mock, db.clone(), clock.clone(), accumulation_timeout);
    //     committer.run().await?;
    //     // No pending tx since we have not accumulated enough data nor did the timeout expire
    //     assert!(!db.has_pending_txs().await?);
    //
    //     clock.adv_time(Duration::from_secs(1)).await;
    //
    //     // when
    //     committer.run().await?;
    //
    //     // then
    //     assert!(db.has_pending_txs().await?);
    //
    //     Ok(())
    // }
}
