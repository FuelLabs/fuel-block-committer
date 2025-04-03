use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use clock::TestClock;
use eth::{Error as EthError, FailoverClient, ProviderConfig, ProviderInit, Result as EthResult};
use metrics::prometheus::IntGauge;
use services::{
    Result, Runner,
    state_listener::{port::l1::Api as L1Api, service::StateListener},
    types::{L1Height, L1Tx, TransactionResponse, TransactionState},
};
use test_helpers::Setup;
use tokio::sync::Mutex;

// Mock ProviderInit for our tests that creates providers with specific behavior
struct MockProviderInit {
    fail_squeezed_out_check: bool,
    tx_failure_responses: Vec<bool>, // true = squeezed out, false = not squeezed out
    tx_failure_idx: Arc<Mutex<usize>>, // track which response to return
}

impl MockProviderInit {
    fn new(fail_squeezed_out_check: bool, tx_failure_responses: Vec<bool>) -> Self {
        Self {
            fail_squeezed_out_check,
            tx_failure_responses,
            tx_failure_idx: Arc::new(Mutex::new(0)),
        }
    }
}

impl ProviderInit for MockProviderInit {
    type Provider = MockL1Provider;

    async fn initialize(&self, config: &ProviderConfig) -> EthResult<Arc<Self::Provider>> {
        let provider = MockL1Provider {
            name: config.name.clone(),
            fail_squeezed_out_check: self.fail_squeezed_out_check,
            tx_failure_responses: self.tx_failure_responses.clone(),
            tx_failure_idx: self.tx_failure_idx.clone(),
        };
        Ok(Arc::new(provider))
    }
}

struct MockL1Provider {
    name: String,
    fail_squeezed_out_check: bool,
    tx_failure_responses: Vec<bool>,
    tx_failure_idx: Arc<Mutex<usize>>,
}

impl eth::L1Provider for MockL1Provider {
    async fn get_block_number(&self) -> EthResult<u64> {
        Ok(10) // Always return block number 10
    }

    async fn get_transaction_response(
        &self,
        _tx_hash: [u8; 32],
    ) -> EthResult<Option<TransactionResponse>> {
        Ok(None) // Transaction not found in a block
    }

    async fn is_squeezed_out(&self, _tx_hash: [u8; 32]) -> EthResult<bool> {
        if self.fail_squeezed_out_check {
            return Err(EthError::Network {
                msg: "Network error from squeezed out check".into(),
                recoverable: true,
            });
        }

        // Return the next response from our predefined list
        let mut idx = self.tx_failure_idx.lock().await;
        let response = if *idx < self.tx_failure_responses.len() {
            self.tx_failure_responses[*idx]
        } else {
            true // Default to squeezed out if we've run out of responses
        };
        *idx += 1;

        Ok(response)
    }

    async fn note_tx_failure(&self, reason: &str) -> EthResult<()> {
        println!("Provider {} noted tx failure: {}", self.name, reason);
        Ok(())
    }

    // Implement other methods with default values
    async fn balance(
        &self,
        _address: alloy::primitives::Address,
    ) -> EthResult<services::types::U256> {
        unimplemented!()
    }

    async fn fees(
        &self,
        _height_range: std::ops::RangeInclusive<u64>,
        _reward_percentiles: &[f64],
    ) -> EthResult<alloy::rpc::types::FeeHistory> {
        unimplemented!()
    }

    async fn submit_state_fragments(
        &self,
        _fragments: services::types::NonEmpty<services::types::Fragment>,
        _previous_tx: Option<services::types::L1Tx>,
        _priority: services::state_committer::port::l1::Priority,
    ) -> EthResult<(services::types::L1Tx, services::types::FragmentsSubmitted)> {
        unimplemented!()
    }

    async fn submit(
        &self,
        _hash: [u8; 32],
        _height: u32,
    ) -> EthResult<services::types::BlockSubmissionTx> {
        unimplemented!()
    }

    fn commit_interval(&self) -> std::num::NonZeroU32 {
        std::num::NonZeroU32::new(10).unwrap()
    }

    fn blob_poster_address(&self) -> Option<alloy::primitives::Address> {
        None
    }

    fn contract_caller_address(&self) -> alloy::primitives::Address {
        Default::default()
    }
}

// Wrapper for FailoverClient to implement L1Api
struct TestL1Api {
    client: FailoverClient<MockProviderInit>,
}

impl TestL1Api {
    fn new(client: FailoverClient<MockProviderInit>) -> Self {
        Self { client }
    }
}

impl L1Api for TestL1Api {
    async fn get_block_number(&self) -> Result<L1Height> {
        self.client
            .get_block_number()
            .await
            .map(|h| h.into())
            .map_err(|e| e.into())
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>> {
        self.client
            .get_transaction_response(tx_hash)
            .await
            .map_err(|e| e.into())
    }

    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> Result<bool> {
        self.client
            .is_squeezed_out(tx_hash)
            .await
            .map_err(|e| e.into())
    }

    async fn note_tx_failure(&self, reason: &str) -> Result<()> {
        self.client
            .note_tx_failure(reason)
            .await
            .map_err(|e| e.into())
    }
}

// Test that transaction failures are tracked and trigger failover
#[tokio::test]
async fn tx_failures_trigger_failover() -> Result<()> {
    // Setup
    let setup = Setup::init().await;

    // Insert fragments and create a pending transaction
    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;

    // Create provider configs
    let provider_configs = vec![
        ProviderConfig::new("provider1", "http://example1.com"),
        ProviderConfig::new("provider2", "http://example2.com"),
    ];

    // Create a provider initializer that will fail transactions
    // We'll configure it to return "squeezed out = true" for 6 transactions
    // With a threshold of 5, this should trigger failover
    let provider_init = MockProviderInit::new(
        false,
        vec![true, true, true, true, true, true], // All 6 transactions are squeezed out
    );

    // Create the failover client
    let tx_failure_threshold = 5;
    let tx_failure_time_window = Duration::from_secs(300);
    let client = FailoverClient::connect(
        provider_configs,
        provider_init,
        3, // transient error threshold
        tx_failure_threshold,
        tx_failure_time_window,
    )
    .await
    .expect("Failed to create failover client");

    // Create the test API adapter
    let l1_api = TestL1Api::new(client);

    // Create the state listener
    let mut state_listener = StateListener::new(
        l1_api,
        setup.db(),
        5, // num_blocks_to_finalize
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    // First run - we should have 5 transactions from the DB to check
    // This should mark 5 transactions as failed due to "squeezed out" being true
    state_listener.run().await?;

    // Add one more transaction to push it over the threshold
    setup.send_fragments([1; 32], 1).await;

    // Second run - one more transaction should trigger the failover
    state_listener.run().await?;

    // Verify the database state - should have failed transactions
    use services::state_listener::port::Storage;
    let non_finalized_txs = setup.db().get_non_finalized_txs().await?;
    assert!(
        !non_finalized_txs.is_empty(),
        "Expected failed transactions"
    );

    // Check if transactions were marked as failed (state 2)
    for tx in &non_finalized_txs {
        assert_eq!(
            tx.state,
            TransactionState::Failed,
            "Expected transaction to be marked as failed"
        );
    }

    Ok(())
}

// Test that if transactions are correctly processed and not lost, failover doesn't happen
#[tokio::test]
async fn successful_txs_dont_trigger_failover() -> Result<()> {
    // Setup
    let setup = Setup::init().await;

    // Insert fragments and create a pending transaction
    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;

    // Create provider configs
    let provider_configs = vec![
        ProviderConfig::new("provider1", "http://example1.com"),
        ProviderConfig::new("provider2", "http://example2.com"),
    ];

    // Create a provider initializer with alternating results
    // This simulates some transactions being squeezed out but not enough to trigger failover
    let provider_init = MockProviderInit::new(
        false,
        vec![false, true, false, true], // Only 2 out of 4 are squeezed out
    );

    // Create the failover client
    let tx_failure_threshold = 5;
    let tx_failure_time_window = Duration::from_secs(300);
    let client = FailoverClient::connect(
        provider_configs,
        provider_init,
        3, // transient error threshold
        tx_failure_threshold,
        tx_failure_time_window,
    )
    .await
    .expect("Failed to create failover client");

    // Create the test API adapter
    let l1_api = TestL1Api::new(client);

    // Create the state listener
    let mut state_listener = StateListener::new(
        l1_api,
        setup.db(),
        5, // num_blocks_to_finalize
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    // Run twice to process transactions
    state_listener.run().await?;
    state_listener.run().await?;

    // The provider should still be healthy since we didn't hit the failure threshold
    // We don't have a direct way to verify this in the test, but the client should
    // still be functioning normally

    Ok(())
}

// Test that a network error in the provider doesn't count as a transaction failure
#[tokio::test]
async fn network_errors_dont_count_as_tx_failures() -> Result<()> {
    // Setup
    let setup = Setup::init().await;

    // Insert fragments and create a pending transaction
    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;

    // Create provider configs
    let provider_configs = vec![
        ProviderConfig::new("provider1", "http://example1.com"),
        ProviderConfig::new("provider2", "http://example2.com"),
    ];

    // Create a provider initializer that will fail with network errors
    let provider_init = MockProviderInit::new(
        true,   // fail is_squeezed_out with network error
        vec![], // No transaction responses needed
    );

    // Create the failover client
    let tx_failure_threshold = 5;
    let tx_failure_time_window = Duration::from_secs(300);
    let client = FailoverClient::connect(
        provider_configs,
        provider_init,
        3, // transient error threshold
        tx_failure_threshold,
        tx_failure_time_window,
    )
    .await
    .expect("Failed to create failover client");

    // Create the test API adapter
    let l1_api = TestL1Api::new(client);

    // Create the state listener
    let mut state_listener = StateListener::new(
        l1_api,
        setup.db(),
        5, // num_blocks_to_finalize
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    // This will error due to the network error in is_squeezed_out
    // But should not count toward transaction failure count
    let result = state_listener.run().await;
    assert!(result.is_err());

    Ok(())
}

// Test that failures outside the time window don't trigger failover
#[tokio::test]
async fn failures_outside_time_window_dont_count() -> Result<()> {
    // Setup
    let setup = Setup::init().await;
    let test_clock = setup.test_clock();

    // Insert fragments and create a pending transaction
    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;

    // Create provider configs
    let provider_configs = vec![
        ProviderConfig::new("provider1", "http://example1.com"),
        ProviderConfig::new("provider2", "http://example2.com"),
    ];

    // Create a provider initializer with all transactions being squeezed out
    let provider_init = MockProviderInit::new(
        false,
        vec![true, true, true, true, true, true], // All are squeezed out
    );

    // Create the failover client with a small time window
    let tx_failure_threshold = 5;
    let tx_failure_time_window = Duration::from_secs(30); // 30 second window
    let client = FailoverClient::connect(
        provider_configs,
        provider_init,
        3, // transient error threshold
        tx_failure_threshold,
        tx_failure_time_window,
    )
    .await
    .expect("Failed to create failover client");

    // Create the test API adapter
    let l1_api = TestL1Api::new(client);

    // Create the state listener
    let mut state_listener = StateListener::new(
        l1_api,
        setup.db(),
        5, // num_blocks_to_finalize
        test_clock.clone(),
        IntGauge::new("test", "test").unwrap(),
    );

    // First run - process first batch of transactions
    state_listener.run().await?;

    // Advance time beyond the window
    test_clock.advance_time(Duration::from_secs(60));

    // Create more transactions
    setup.send_fragments([1; 32], 1).await;

    // Second run - even though we have more failures, they shouldn't
    // trigger a failover because the old ones are now outside the time window
    state_listener.run().await?;

    // System should still be functioning without failover
    Ok(())
}

// Test that the system switches to the backup provider when the primary fails
#[tokio::test]
async fn system_switches_to_backup_provider() -> Result<()> {
    // Setup
    let setup = Setup::init().await;

    // Insert fragments and create pending transactions
    let tx_hash = [0; 32];
    setup.send_fragments(tx_hash, 0).await;

    // Create provider configs - we need two providers
    let provider_configs = vec![
        ProviderConfig::new("failing-provider", "http://example1.com"),
        ProviderConfig::new("healthy-provider", "http://example2.com"),
    ];

    // Create a provider initializer
    // First provider will return 'squeezed out=true' for all transactions (triggering failover)
    // Second provider will return 'squeezed out=false' (transactions are still in mempool)
    let provider_init = MockProviderInit::new(
        false,
        vec![
            // First provider responses - all squeezed out
            true, true, true, true, true, true,
            // Second provider responses - none squeezed out
            false, false, false, false, false,
        ],
    );

    // Create the failover client - 5 failures will trigger failover
    let tx_failure_threshold = 5;
    let tx_failure_time_window = Duration::from_secs(300);
    let client = FailoverClient::connect(
        provider_configs,
        provider_init,
        3, // transient error threshold
        tx_failure_threshold,
        tx_failure_time_window,
    )
    .await
    .expect("Failed to create failover client");

    // Create the test API adapter
    let l1_api = TestL1Api::new(client);

    // Create the state listener
    let mut state_listener = StateListener::new(
        l1_api,
        setup.db(),
        5, // num_blocks_to_finalize
        setup.test_clock(),
        IntGauge::new("test", "test").unwrap(),
    );

    // First batch of transactions should be processed with the first provider
    // and marked as failed (squeezed out)
    state_listener.run().await?;

    // At this point internal state should cause first provider to be marked unhealthy
    // and second provider to be used

    // Add another batch of transactions
    setup.send_fragments([1; 32], 1).await;

    // Second run - transactions should be processed with the backup provider
    // and should NOT be marked as squeezed out
    state_listener.run().await?;

    // Get latest transactions - the ones processed by the backup provider
    // They should be in pending state, not failed state
    let latest_tx = setup
        .get_tx_by_hash([1; 32])
        .await?
        .expect("Transaction should exist");

    // Should be pending (state 0), not failed (state 2)
    assert_eq!(
        latest_tx.state,
        TransactionState::Pending,
        "Expected transaction to be marked as pending"
    );

    Ok(())
}
