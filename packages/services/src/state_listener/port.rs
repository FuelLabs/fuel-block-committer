
use crate::{
    types::{DateTime, EigenDASubmission, EthereumDASubmission, TransactionCostUpdate, TransactionState, Utc},
    Result,
};

pub mod l1 {
    use crate::{
        types::{L1Height, TransactionResponse},
        Result,
    };

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    #[cfg_attr(feature = "test-helpers", mockall::automock)]
    pub trait Api {
        async fn get_block_number(&self) -> Result<L1Height>;
        async fn get_transaction_response(
            &self,
            tx_hash: [u8; 32],
        ) -> Result<Option<TransactionResponse>>;
        async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool>;
    }
}

pub mod eigen_da {
    use crate::{
        types::DispersalStatus, Result
    };

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    #[cfg_attr(feature = "test-helpers", mockall::automock)]
    pub trait Api {
        async fn get_blob_status(&self, id: Vec<u8>) -> Result<DispersalStatus>;
    }
}

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
pub trait Storage: Sync {
    async fn get_non_finalized_txs(&self) -> Result<Vec<EthereumDASubmission>>;
    async fn get_non_finalized_submissions(&self) -> Result<Vec<EigenDASubmission>>; // TODO
    async fn update_tx_states_and_costs(
        &self,
        selective_changes: Vec<([u8; 32], TransactionState)>,
        noncewide_changes: Vec<([u8; 32], u32, TransactionState)>,
        cost_per_tx: Vec<TransactionCostUpdate>,
    ) -> Result<()>;
    async fn has_pending_txs(&self) -> Result<bool>;
    async fn earliest_submission_attempt(&self, nonce: u32) -> Result<Option<DateTime<Utc>>>;
}

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
}
