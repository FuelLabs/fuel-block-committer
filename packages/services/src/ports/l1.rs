use std::num::NonZeroUsize;

use crate::{
    types::{
        BlockSubmissionTx, Fragment, L1Height, L1Tx, NonEmpty, NonNegative, TransactionResponse,
        U256,
    },
    Result,
};

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Contract: Send + Sync {
    async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;
    fn commit_interval(&self) -> std::num::NonZeroU32;
}

#[derive(Debug, Clone, Copy)]
pub struct FragmentsSubmitted {
    pub num_fragments: NonZeroUsize,
}

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Api {
    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<L1Tx>,
    ) -> Result<(L1Tx, FragmentsSubmitted)>;
    async fn get_block_number(&self) -> Result<L1Height>;
    async fn balance(&self, address: crate::types::Address) -> Result<U256>;
    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>>;
    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool>;
}

pub trait FragmentEncoder {
    fn encode(&self, data: NonEmpty<u8>, id: NonNegative<i32>) -> Result<NonEmpty<Fragment>>;
    fn gas_usage(&self, num_bytes: NonZeroUsize) -> u64;
}
