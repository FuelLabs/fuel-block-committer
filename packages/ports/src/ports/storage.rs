use std::{
    collections::{BTreeSet, HashSet},
    sync::Arc,
};

use futures::SinkExt;
use sqlx::types::chrono::{DateTime, Utc};

use crate::types::{BlockSubmission, L1Tx, NonNegative, StateSubmission, TransactionState};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("db response: {0}")]
    Database(String),
    #[error("data conversion app<->db failed: {0}")]
    Conversion(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuelBlock {
    pub hash: [u8; 32],
    pub height: u32,
    pub data: Vec<u8>,
}

impl From<crate::fuel::FuelBlock> for FuelBlock {
    fn from(value: crate::fuel::FuelBlock) -> Self {
        let data = value
            .transactions
            .into_iter()
            .flat_map(|tx| tx.into_iter())
            .collect();
        Self {
            hash: *value.id,
            height: value.header.height,
            data,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRoster {
    missing_block_heights: Vec<u32>,
    highest_block_present: Option<u32>,
}

impl BlockRoster {
    pub fn new(missing_block_heights: Vec<u32>, highest_block_present: Option<u32>) -> Self {
        Self {
            missing_block_heights,
            highest_block_present,
        }
    }

    pub fn missing_block_heights(&self, current_height: u32, lower_cutoff: u32) -> BTreeSet<u32> {
        let mut missing = BTreeSet::from_iter(self.missing_block_heights.clone());

        if let Some(highest_block_present) = self.highest_block_present {
            missing.extend((highest_block_present + 1)..=current_height);
        } else {
            missing.extend(lower_cutoff..=current_height)
        }

        missing.retain(|&height| height >= lower_cutoff);

        missing
    }
}

#[async_trait::async_trait]
#[impl_tools::autoimpl(for<T: trait> &T, &mut T, Arc<T>, Box<T>)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Storage: Send + Sync {
    async fn insert(&self, submission: BlockSubmission) -> Result<()>;
    async fn submission_w_latest_block(&self) -> Result<Option<BlockSubmission>>;
    async fn set_submission_completed(&self, fuel_block_hash: [u8; 32]) -> Result<BlockSubmission>;
    async fn insert_block(&self, block: FuelBlock) -> Result<()>;
    async fn block_available(&self, hash: &[u8; 32]) -> Result<bool>;
    async fn all_blocks(&self) -> Result<Vec<FuelBlock>>;
    async fn block_roster(&self) -> Result<BlockRoster>;
    async fn insert_bundle_and_fragments(
        &self,
        bundle_blocks: &[[u8; 32]],
        fragments: Vec<Vec<u8>>,
    ) -> Result<()>;

    // async fn insert_state_submission(&self, submission: StateSubmission) -> Result<()>;
    // fn stream_unfinalized_segment_data<'a>(
    //     &'a self,
    // ) -> Pin<Box<dyn Stream<Item = Result<UnfinalizedSubmissionData>> + 'a + Send>>;
    // async fn record_pending_tx(
    //     &self,
    //     tx_hash: [u8; 32],
    //     fragments: Vec<StateFragment>,
    // ) -> Result<()>;
    async fn get_pending_txs(&self) -> Result<Vec<L1Tx>>;
    async fn has_pending_txs(&self) -> Result<bool>;
    async fn state_submission_w_latest_block(&self) -> Result<Option<StateSubmission>>;
    async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
    async fn update_submission_tx_state(
        &self,
        hash: [u8; 32],
        state: TransactionState,
    ) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! set {
        ( $( $x:expr ),* ) => {
            {
            let mut set = std::collections::BTreeSet::new();
            $(
                set.insert($x);
            )*
            set
            }
        };
    }

    #[test]
    fn reports_no_missing_blocks() {
        // given
        let roster = BlockRoster::new(vec![], Some(10));

        // when
        let missing = roster.missing_block_heights(10, 0);

        // then
        assert!(missing.is_empty());
    }

    #[test]
    fn reports_what_the_db_gave() {
        // given
        let roster = BlockRoster::new(vec![1, 2, 3], Some(10));

        // when
        let missing = roster.missing_block_heights(10, 0);

        // then
        assert_eq!(missing, set![1, 2, 3]);
    }

    #[test]
    fn reports_missing_blocks_if_latest_height_doest_match_with_highest_db_block() {
        // given
        let roster = BlockRoster::new(vec![1, 2, 3], Some(10));

        // when
        let missing = roster.missing_block_heights(12, 0);

        // then
        assert_eq!(missing, set![1, 2, 3, 11, 12]);
    }

    #[test]
    fn wont_report_below_cutoff() {
        // given
        let roster = BlockRoster::new(vec![1, 2, 3], Some(10));

        // when
        let missing = roster.missing_block_heights(12, 10);

        // then
        assert_eq!(missing, set![11, 12]);
    }

    #[test]
    fn no_block_was_imported_ie_initial_db_state() {
        // given
        let roster = BlockRoster::new(vec![], None);

        // when
        let missing = roster.missing_block_heights(10, 3);

        // then
        assert_eq!(missing, set![3, 4, 5, 6, 7, 8, 9, 10]);
    }
}
