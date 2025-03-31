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
pub type ProviderFactory = Box<
    dyn Fn(&ProviderConfig) -> BoxFuture<'static, EthResult<Arc<dyn L1Provider>>>
        + Send
        + Sync
        + 'static,
>;

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

/// Creates a factory that produces real WebsocketClient instances
pub fn create_real_provider_factory(
    contract_address: Address,
    signers: crate::websocket::Signers,
    unhealthy_after_n_errors: usize,
    tx_config: TxConfig,
) -> ProviderFactory {
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
                signers.clone(),
                unhealthy_after_n_errors,
                tx_config,
            )
            .await
            .map_err(|e| EthError::Other(format!("Failed to connect: {}", e)))?;

            Ok(Arc::new(client) as Arc<dyn L1Provider>)
        })
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    /// A mock implementation of L1Provider for testing
    #[derive(Debug)]
    pub struct MockL1Provider {
        id: String,
        call_counts: Mutex<HashMap<String, usize>>,
        responses: Mutex<VecDeque<EthResult<MockResponseType>>>,
    }

    /// Types of responses the mock can return
    #[derive(Clone, Debug)]
    pub enum MockResponseType {
        BlockNumber(u64),
        TxHash(TxHash),
        TransactionResponse(Option<TransactionResponse>),
        SqueezedOut(bool),
        Balance(U256),
        FeeHistory(FeeHistory),
        StateFragments((L1Tx, FragmentsSubmitted)),
        BlockSubmission(BlockSubmissionTx),
    }

    impl MockL1Provider {
        /// Create a new MockL1Provider with the given ID and pre-programmed responses
        pub fn new(id: &str, responses: Vec<EthResult<MockResponseType>>) -> Arc<Self> {
            Arc::new(Self {
                id: id.to_string(),
                call_counts: Mutex::new(HashMap::new()),
                responses: Mutex::new(responses.into()),
            })
        }

        /// Get the next response from the queue
        async fn next_response(&self) -> Option<EthResult<MockResponseType>> {
            self.responses.lock().await.pop_front()
        }

        /// Record a method call
        async fn record_call(&self, method_name: &str) {
            let mut counts = self.call_counts.lock().await;
            *counts.entry(method_name.to_string()).or_insert(0) += 1;
        }

        /// Get the call count for a method
        pub async fn get_call_count(&self, method_name: &str) -> usize {
            self.call_counts
                .lock()
                .await
                .get(method_name)
                .cloned()
                .unwrap_or(0)
        }
    }

    #[async_trait]
    impl L1Provider for MockL1Provider {
        async fn get_block_number(&self) -> EthResult<u64> {
            self.record_call("get_block_number").await;
            tracing::debug!("Mock [{}] handling get_block_number", self.id);

            match self.next_response().await {
                Some(Ok(MockResponseType::BlockNumber(n))) => Ok(n),
                Some(Err(e)) => Err(e),
                Some(Ok(other)) => panic!(
                    "Mock [{}] expected BlockNumber but got {:?}",
                    self.id, other
                ),
                None => panic!(
                    "Mock [{}] ran out of responses for get_block_number",
                    self.id
                ),
            }
        }

        async fn get_transaction_response(
            &self,
            _tx_hash: [u8; 32],
        ) -> EthResult<Option<TransactionResponse>> {
            self.record_call("get_transaction_response").await;
            tracing::debug!("Mock [{}] handling get_transaction_response", self.id);

            match self.next_response().await {
                Some(Ok(MockResponseType::TransactionResponse(r))) => Ok(r),
                Some(Err(e)) => Err(e),
                Some(Ok(other)) => panic!(
                    "Mock [{}] expected TransactionResponse but got {:?}",
                    self.id, other
                ),
                None => panic!(
                    "Mock [{}] ran out of responses for get_transaction_response",
                    self.id
                ),
            }
        }

        async fn is_squeezed_out(&self, _tx_hash: [u8; 32]) -> EthResult<bool> {
            self.record_call("is_squeezed_out").await;
            tracing::debug!("Mock [{}] handling is_squeezed_out", self.id);

            match self.next_response().await {
                Some(Ok(MockResponseType::SqueezedOut(b))) => Ok(b),
                Some(Err(e)) => Err(e),
                Some(Ok(other)) => panic!(
                    "Mock [{}] expected SqueezedOut but got {:?}",
                    self.id, other
                ),
                None => panic!(
                    "Mock [{}] ran out of responses for is_squeezed_out",
                    self.id
                ),
            }
        }

        async fn balance(&self, _address: Address) -> EthResult<U256> {
            self.record_call("balance").await;
            tracing::debug!("Mock [{}] handling balance", self.id);

            match self.next_response().await {
                Some(Ok(MockResponseType::Balance(b))) => Ok(b),
                Some(Err(e)) => Err(e),
                Some(Ok(other)) => {
                    panic!("Mock [{}] expected Balance but got {:?}", self.id, other)
                }
                None => panic!("Mock [{}] ran out of responses for balance", self.id),
            }
        }

        async fn fees(
            &self,
            _height_range: RangeInclusive<u64>,
            _reward_percentiles: &[f64],
        ) -> EthResult<FeeHistory> {
            self.record_call("fees").await;
            tracing::debug!("Mock [{}] handling fees", self.id);

            match self.next_response().await {
                Some(Ok(MockResponseType::FeeHistory(f))) => Ok(f),
                Some(Err(e)) => Err(e),
                Some(Ok(other)) => {
                    panic!("Mock [{}] expected FeeHistory but got {:?}", self.id, other)
                }
                None => panic!("Mock [{}] ran out of responses for fees", self.id),
            }
        }

        async fn submit_state_fragments(
            &self,
            _fragments: NonEmpty<Fragment>,
            _previous_tx: Option<services::types::L1Tx>,
            _priority: Priority,
        ) -> EthResult<(L1Tx, FragmentsSubmitted)> {
            self.record_call("submit_state_fragments").await;
            tracing::debug!("Mock [{}] handling submit_state_fragments", self.id);

            match self.next_response().await {
                Some(Ok(MockResponseType::StateFragments(r))) => Ok(r),
                Some(Err(e)) => Err(e),
                Some(Ok(other)) => panic!(
                    "Mock [{}] expected StateFragments but got {:?}",
                    self.id, other
                ),
                None => panic!(
                    "Mock [{}] ran out of responses for submit_state_fragments",
                    self.id
                ),
            }
        }

        async fn submit(&self, _hash: [u8; 32], _height: u32) -> EthResult<BlockSubmissionTx> {
            self.record_call("submit").await;
            tracing::debug!("Mock [{}] handling submit", self.id);

            match self.next_response().await {
                Some(Ok(MockResponseType::BlockSubmission(b))) => Ok(b),
                Some(Err(e)) => Err(e),
                Some(Ok(other)) => panic!(
                    "Mock [{}] expected BlockSubmission but got {:?}",
                    self.id, other
                ),
                None => panic!("Mock [{}] ran out of responses for submit", self.id),
            }
        }
    }

    /// Creates a mock provider factory for testing
    pub fn create_mock_provider_factory(
        mock_responses: Arc<Mutex<HashMap<String, Vec<EthResult<MockResponseType>>>>>,
    ) -> ProviderFactory {
        Box::new(move |config: &ProviderConfig| {
            let responses_map_clone = mock_responses.clone();
            let config_clone = config.clone();

            Box::pin(async move {
                tracing::debug!("Mock factory called for config: {:?}", config_clone);
                let mut map = responses_map_clone.lock().await;

                // Get the pre-programmed responses for this specific URL
                let responses = map.remove(&config_clone.url).unwrap_or_else(|| {
                    panic!(
                        "Mock factory: No responses configured for URL {}",
                        config_clone.url
                    );
                });

                // Create the mock provider instance for this config
                let provider = MockL1Provider::new(&config_clone.url, responses);
                Ok(provider as Arc<dyn L1Provider>)
            })
        })
    }
}
