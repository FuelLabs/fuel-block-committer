use std::{fmt::Display, num::NonZeroU32, ops::RangeInclusive};

use alloy::{primitives::Address, rpc::types::FeeHistory};
use delegate::delegate;
use services::{
    state_committer::port::l1::Priority,
    types::{
        BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse, U256,
    },
};

use crate::error::Result;

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
#[cfg_attr(test, mockall::automock)]
pub trait L1Provider: Send + Sync {
    /// Get the current block number from the L1 network
    async fn get_block_number(&self) -> Result<u64>;

    /// Get the transaction response for a given transaction hash
    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>>;

    /// Check if a transaction has been squeezed out of the mempool
    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool>;

    /// Get the balance of an address
    async fn balance(&self, address: Address) -> Result<U256>;

    /// Get fee history for a range of blocks
    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> Result<FeeHistory>;

    /// Submit state fragments to L1
    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<services::types::L1Tx>,
        priority: Priority,
    ) -> Result<(L1Tx, FragmentsSubmitted)>;

    /// Submit a block hash and height
    async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;

    /// Note that a transaction has failed due to provider issues
    async fn note_tx_failure(&self, reason: &str) -> Result<()>;

    fn commit_interval(&self) -> NonZeroU32;

    fn blob_poster_address(&self) -> Option<Address>;

    fn contract_caller_address(&self) -> Address;
}

/// Blanket implementation for references to types that implement L1Provider
impl<T: L1Provider + ?Sized> L1Provider for &T {
    delegate! {
        to (*self) {
                async fn get_block_number(&self) -> Result<u64>;

                async fn get_transaction_response(
                    &self,
                    tx_hash: [u8; 32],
                ) -> Result<Option<TransactionResponse>>;

                async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool>;

                async fn balance(&self, address: Address) -> Result<U256>;

                async fn fees(
                    &self,
                    height_range: RangeInclusive<u64>,
                    reward_percentiles: &[f64],
                ) -> Result<FeeHistory>;

                async fn submit_state_fragments(
                    &self,
                    fragments: NonEmpty<Fragment>,
                    previous_tx: Option<services::types::L1Tx>,
                    priority: Priority,
                ) -> Result<(L1Tx, FragmentsSubmitted)>;

                async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;

                async fn note_tx_failure(&self, reason: &str) -> Result<()>;

                fn commit_interval(&self) -> NonZeroU32;

                fn blob_poster_address(&self) -> Option<Address>;

                fn contract_caller_address(&self) -> Address;
        }
    }
}
