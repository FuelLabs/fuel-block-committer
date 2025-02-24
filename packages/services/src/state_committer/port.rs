use nonempty::NonEmpty;

use crate::{
    types::{storage::BundleFragment, DateTime, L1Tx, NonNegative, Utc},
    Error, Result,
};

pub mod l1 {
    use nonempty::NonEmpty;

    use crate::{
        types::{BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx},
        Error, Result,
    };
    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    #[cfg_attr(feature = "test-helpers", mockall::automock)]
    pub trait Contract: Sync {
        async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;
    }

    #[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
    pub struct Priority(f64);

    impl Priority {
        pub const MIN: Self = Self(0.);
        pub const MAX: Self = Self(100.);

        pub fn new(percent: f64) -> Result<Self> {
            if !(0. ..=100.).contains(&percent) {
                return Err(Error::Other(
                    "priority must be between 0 and 100".to_string(),
                ));
            }

            Ok(Self(percent))
        }

        pub fn get(&self) -> f64 {
            self.0
        }
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    #[cfg_attr(feature = "test-helpers", mockall::automock)]
    pub trait Api {
        async fn current_height(&self) -> Result<u64>;
        async fn submit_state_fragments(
            &self,
            fragments: NonEmpty<Fragment>,
            previous_tx: Option<L1Tx>,
            priority: Priority,
        ) -> Result<(L1Tx, FragmentsSubmitted)>;
    }
}

pub mod fuel {

    use crate::Result;

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    #[cfg_attr(feature = "test-helpers", mockall::automock)]
    pub trait Api: Sync {
        async fn latest_height(&self) -> Result<u32>;
    }
}

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
pub trait Storage: Sync {
    async fn has_nonfinalized_txs(&self) -> Result<bool>;
    async fn last_time_a_fragment_was_finalized(&self) -> Result<Option<DateTime<Utc>>>;
    async fn record_pending_tx(
        &self,
        tx: L1Tx,
        fragment_id: NonEmpty<NonNegative<i32>>,
        created_at: DateTime<Utc>,
    ) -> Result<()>;
    async fn oldest_nonfinalized_fragments(
        &self,
        starting_height: u32,
        limit: usize,
    ) -> Result<Vec<BundleFragment>>;
    async fn latest_bundled_height(&self) -> Result<Option<u32>>;
    async fn fragments_submitted_by_tx(&self, tx_hash: [u8; 32]) -> Result<Vec<BundleFragment>>;
    async fn get_latest_pending_txs(&self) -> Result<Option<L1Tx>>;
}

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
    fn elapsed(&self, since: DateTime<Utc>) -> Result<std::time::Duration> {
        self.now()
            .signed_duration_since(since)
            .to_std()
            .map_err(|e| Error::Other(format!("failed to convert time: {e}")))
    }
}
