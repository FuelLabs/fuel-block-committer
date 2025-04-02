use alloy::primitives::{Address, Bytes, TxHash};
use alloy::rpc::types::FeeHistory;
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{Error as EthError, Result as EthResult};
use crate::{L1Keys, TxConfig, WebsocketClient};
use services::state_committer::port::l1::Priority;
use services::types::{
    BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx, NonEmpty, TransactionResponse, U256,
};

/// Trait defining the necessary L1 provider operations
/// This is implemented by WebsocketClient and mocks for testing
#[cfg_attr(test, mockall::automock)]
#[async_trait]
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

/// A struct that holds configuration for a provider
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    /// The URL to connect to
    pub url: String,
    /// Optional name for logging purposes
    pub name: Option<String>,
}

impl ProviderConfig {
    /// Create a new ProviderConfig with a URL
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            name: None,
        }
    }

    /// Create a new ProviderConfig with a URL and name
    pub fn with_name(url: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            name: Some(name.into()),
        }
    }

    /// Get a display name for this provider, either the configured name or the URL
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.url)
    }
}

/// A factory that can create an L1Provider from a ProviderConfig
pub type ProviderFactory<P> =
    Box<dyn Fn(&ProviderConfig) -> BoxFuture<'static, EthResult<Arc<P>>> + Send + Sync + 'static>;

/// Implementation of L1Provider for WebsocketClient
#[async_trait]
impl L1Provider for WebsocketClient {
    async fn get_block_number(&self) -> EthResult<u64> {
        Ok(self._get_block_number().await?)
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> EthResult<Option<TransactionResponse>> {
        self.get_transaction_response(tx_hash).await
    }

    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> EthResult<bool> {
        self.is_squeezed_out(tx_hash).await
    }

    async fn balance(&self, address: Address) -> EthResult<U256> {
        self.balance(address).await
    }

    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
        reward_percentiles: &[f64],
    ) -> EthResult<FeeHistory> {
        self.fees(height_range, reward_percentiles).await
    }

    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<services::types::L1Tx>,
        priority: Priority,
    ) -> EthResult<(L1Tx, FragmentsSubmitted)> {
        self.submit_state_fragments(fragments, previous_tx, priority)
            .await
    }

    async fn submit(&self, hash: [u8; 32], height: u32) -> EthResult<BlockSubmissionTx> {
        self.submit(hash, height).await
    }
}

/// Blanket implementation for references to types that implement L1Provider
#[async_trait]
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

/// Creates a factory that produces real WebsocketClient instances
pub fn create_real_provider_factory(
    contract_address: Address,
    signers: Arc<crate::websocket::Signers>,
    unhealthy_after_n_errors: usize,
    tx_config: TxConfig,
) -> ProviderFactory<WebsocketClient> {
    Box::new(move |config: &ProviderConfig| {
        let contract_address = contract_address;
        let signers = signers.clone();
        let tx_config = tx_config.clone();
        let url_str = config.url.clone();

        Box::pin(async move {
            use url::Url;

            // Parse the URL
            let url =
                Url::parse(&url_str).map_err(|e| EthError::Other(format!("Invalid URL: {}", e)))?;

            // Create the WebsocketClient
            let client = WebsocketClient::connect(
                url,
                contract_address,
                (*signers).clone(),
                unhealthy_after_n_errors,
                tx_config,
            )
            .await
            .map_err(|e| EthError::Other(format!("Failed to connect: {}", e)))?;

            Ok(Arc::new(client))
        })
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Creates a mock provider factory for testing
    pub fn create_mock_provider_factory(
        providers: Vec<(String, MockL1Provider)>,
    ) -> ProviderFactory<MockL1Provider> {
        // Create a map of providers
        let provider_map: HashMap<String, Arc<MockL1Provider>> = providers
            .into_iter()
            .map(|(url, provider)| (url, Arc::new(provider)))
            .collect();

        // Wrap in an Arc to allow sharing
        let shared_map = Arc::new(provider_map);

        Box::new(move |config: &ProviderConfig| {
            let providers = shared_map.clone();
            let url = config.url.clone();

            Box::pin(async move {
                // Get a reference to the provider
                let provider = providers
                    .get(&url)
                    .ok_or_else(|| {
                        EthError::Other(format!("No mock provider configured for URL: {}", url))
                    })?
                    .clone();

                Ok(provider)
            })
        })
    }
}
