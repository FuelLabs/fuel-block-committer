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
    async fn prepare_fragments(&self) -> Result<(Vec<u32>, Vec<u8>)> {
        let fragments = self.storage.get_unsubmitted_fragments().await?;

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
        let (fragment_ids, data) = self.prepare_fragments().await?;
        if fragment_ids.is_empty() {
            return Ok(());
        }

        let tx_hash = self.l1_adapter.submit_l2_state(data).await?;
        self.storage
            .record_pending_tx(tx_hash, fragment_ids)
            .await?;

        Ok(())
    }

    async fn is_tx_pending(&self) -> Result<bool> {
        self.storage.has_pending_txs().await.map_err(|e| e.into())
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
    use mockall::predicate;
    use ports::types::{L1Height, StateFragment, StateSubmission, TransactionResponse, U256};
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

    fn given_l1_that_expects_submission(fragment: StateFragment) -> MockL1 {
        let mut l1 = MockL1::new();

        l1.api
            .expect_submit_l2_state()
            .with(predicate::eq(fragment.data))
            .return_once(move |_| Ok([1u8; 32]));

        l1
    }

    fn given_state() -> (StateSubmission, StateFragment) {
        (
            StateSubmission {
                id: None,
                block_hash: [0u8; 32],
                block_height: 1,
            },
            StateFragment {
                id: None,
                submission_id: None,
                fragment_idx: 0,
                data: vec![1, 2, 3],
                created_at: ports::types::Utc::now(),
            },
        )
    }

    #[tokio::test]
    async fn test_submit_state() -> Result<()> {
        // given
        let (state, fragment) = given_state();
        let l1_mock = given_l1_that_expects_submission(fragment.clone());

        let process = PostgresProcess::shared().await.unwrap();
        let db = process.create_random_db().await?;
        db.insert_state_submission(state, vec![fragment]).await?;
        let mut committer = StateCommitter::new(l1_mock, db.clone());

        // when
        committer.run().await.unwrap();

        // then
        assert!(db.has_pending_txs().await?);

        Ok(())
    }
}
