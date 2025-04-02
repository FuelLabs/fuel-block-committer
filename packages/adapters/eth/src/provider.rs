use alloy::primitives::Address;
use alloy::rpc::types::FeeHistory;
use std::ops::RangeInclusive;
use std::sync::Arc;
use url::Url;

use crate::error::{Error as EthError, Result as EthResult};
use crate::failover_client::{ProviderConfig, ProviderInit};
use crate::{TxConfig, WebsocketClient};
use services::state_committer::port::l1::Priority;
use services::types::{
    BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse, U256,
};

#[allow(async_fn_in_trait)]
#[trait_variant::make(Send)]
#[cfg_attr(test, mockall::automock)]
pub trait L1Provider: Send + Sync {
    /// Get the current block number from the L1 network
    async fn get_block_number(&self) -> EthResult<u64>;

    /// Get the transaction response for a given transaction hash
    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> EthResult<Option<TransactionResponse>>;

    /// Check if a transaction has been squeezed out of the mempool
    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> EthResult<bool>;

    /// Get the balance of an address
    async fn balance(&self, address: Address) -> EthResult<U256>;

    /// Get fee history for a range of blocks
    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> EthResult<FeeHistory>;

    /// Submit state fragments to L1
    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<services::types::L1Tx>,
        priority: Priority,
    ) -> EthResult<(L1Tx, FragmentsSubmitted)>;

    /// Submit a block hash and height
    async fn submit(&self, hash: [u8; 32], height: u32) -> EthResult<BlockSubmissionTx>;
}

/// Blanket implementation for references to types that implement L1Provider
impl<T: L1Provider + ?Sized> L1Provider for &T {
    async fn get_block_number(&self) -> EthResult<u64> {
        (*self).get_block_number().await
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> EthResult<Option<TransactionResponse>> {
        (*self).get_transaction_response(tx_hash).await
    }

    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> EthResult<bool> {
        (*self).is_squeezed_out(tx_hash).await
    }

    async fn balance(&self, address: Address) -> EthResult<U256> {
        (*self).balance(address).await
    }

    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> EthResult<FeeHistory> {
        (*self).fees(height_range, reward_percentiles).await
    }

    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<services::types::L1Tx>,
        priority: Priority,
    ) -> EthResult<(L1Tx, FragmentsSubmitted)> {
        (*self)
            .submit_state_fragments(fragments, previous_tx, priority)
            .await
    }

    async fn submit(&self, hash: [u8; 32], height: u32) -> EthResult<BlockSubmissionTx> {
        (*self).submit(hash, height).await
    }
}
