pub mod service {
    use std::collections::HashSet;

    use metrics::{
        RegistersMetrics,
        prometheus::{IntGauge, Opts, core::Collector},
    };
    use tracing::{info, warn};

    use crate::{
        Runner,
        types::{L1Tx, TransactionCostUpdate, TransactionState},
    };

    pub struct StateListener<L1, Db, Clock> {
        l1_adapter: L1,
        storage: Db,
        num_blocks_to_finalize: u64,
        metrics: Metrics,
        clock: Clock,
    }

    impl<L1, Db, Clock> StateListener<L1, Db, Clock> {
        pub fn new(
            l1_adapter: L1,
            storage: Db,
            num_blocks_to_finalize: u64,
            clock: Clock,
            last_finalization_time_metric: IntGauge,
        ) -> Self {
            Self {
                l1_adapter,
                storage,
                num_blocks_to_finalize,
                metrics: Metrics::new(last_finalization_time_metric),
                clock,
            }
        }
    }

    impl<L1, Db, Clock> StateListener<L1, Db, Clock>
    where
        L1: crate::state_listener::port::l1::Api,
        Db: crate::state_listener::port::Storage,
        Clock: crate::state_listener::port::Clock,
    {
        async fn check_non_finalized_txs(&self, non_finalized_txs: Vec<L1Tx>) -> crate::Result<()> {
            let current_block_number: u64 = self.l1_adapter.get_block_number().await?.into();

            // we need to accumulate all the changes and then update the db atomically
            // to avoid race conditions with other services
            let mut skip_nonces = HashSet::new();
            let mut selective_change = vec![];
            let mut noncewide_changes = vec![];

            let mut cost_per_tx = vec![];

            for tx in non_finalized_txs {
                if skip_nonces.contains(&tx.nonce) {
                    continue;
                }

                // get response if tx is included in a block
                let Some(tx_response) = self.l1_adapter.get_transaction_response(tx.hash).await?
                else {
                    // not included in block - check what happened to the tx

                    match (tx.state, self.l1_adapter.is_squeezed_out(tx.hash).await?) {
                        (TransactionState::Pending | TransactionState::IncludedInBlock, true) => {
                            // not in the mempool anymore set it to failed
                            selective_change.push((tx.hash, tx.nonce, TransactionState::Failed));

                            // Note the transaction failure for provider tracking
                            if let Err(e) = self
                                .l1_adapter
                                .note_tx_failure(&format!(
                                    "Transaction {} not found in mempool",
                                    hex::encode(tx.hash)
                                ))
                                .await
                            {
                                warn!("Failed to note transaction failure: {}", e);
                            }

                            info!(
                                "blob tx {} not found in mempool. Setting to failed",
                                hex::encode(tx.hash)
                            );
                        }

                        (TransactionState::IncludedInBlock, false) => {
                            // if tx was in block and reorg happened now it is in the mempool - we need to set the tx to pending
                            selective_change.push((tx.hash, tx.nonce, TransactionState::Pending));

                            info!(
                                "blob tx {} returned to mempool. Setting to pending",
                                hex::encode(tx.hash)
                            );
                        }
                        _ => {}
                    }

                    continue;
                };

                skip_nonces.insert(tx.nonce);

                if !tx_response.succeeded() {
                    // set tx to failed all txs with the same nonce to failed
                    noncewide_changes.push((tx.hash, tx.nonce, TransactionState::Failed));

                    // Note: We don't count actual execution failures as provider issues
                    // The transaction was properly included but failed for other reasons

                    info!("failed blob tx {}", hex::encode(tx.hash));
                    continue;
                }

                if current_block_number.saturating_sub(tx_response.block_number())
                    < self.num_blocks_to_finalize
                {
                    // tx included in block but is not yet finalized
                    if tx.state == TransactionState::Pending {
                        // set tx to included and all txs with the same nonce to failed
                        noncewide_changes.push((
                            tx.hash,
                            tx.nonce,
                            TransactionState::IncludedInBlock,
                        ));

                        info!(
                            "blob tx {} included in block {}",
                            hex::encode(tx.hash),
                            tx_response.block_number()
                        );
                    }

                    continue;
                }

                // st tx to finalized and all txs with the same nonce to failed
                let now = self.clock.now();
                noncewide_changes.push((tx.hash, tx.nonce, TransactionState::Finalized(now)));
                cost_per_tx.push(TransactionCostUpdate {
                    tx_hash: tx.hash,
                    total_fee: tx_response.total_fee(),
                    da_block_height: tx_response.block_number(),
                });

                self.metrics.last_finalization_time.set(now.timestamp());

                info!("blob tx {} finalized", hex::encode(tx.hash));

                let earliest_submission_attempt =
                    self.storage.earliest_submission_attempt(tx.nonce).await?;

                self.metrics.last_finalization_interval.set(
                    earliest_submission_attempt
                        .map(|earliest_submission_attempt| {
                            (now - earliest_submission_attempt).num_seconds()
                        })
                        .unwrap_or(0),
                );

                self.metrics
                    .last_eth_block_w_blob
                    .set(i64::try_from(tx_response.block_number()).unwrap_or(i64::MAX));
            }

            selective_change.retain(|(_, nonce, _)| !skip_nonces.contains(nonce));
            let selective_change: Vec<_> = selective_change
                .into_iter()
                .map(|(hash, _, state)| (hash, state))
                .collect();

            self.storage
                .update_tx_states_and_costs(selective_change, noncewide_changes, cost_per_tx)
                .await?;

            Ok(())
        }
    }

    impl<L1, Db, Clock> Runner for StateListener<L1, Db, Clock>
    where
        L1: crate::state_listener::port::l1::Api + Send + Sync,
        Db: crate::state_listener::port::Storage,
        Clock: crate::state_listener::port::Clock + Send + Sync,
    {
        async fn run(&mut self) -> crate::Result<()> {
            let non_finalized_txs = self.storage.get_non_finalized_txs().await?;

            if non_finalized_txs.is_empty() {
                return Ok(());
            }

            self.check_non_finalized_txs(non_finalized_txs).await?;

            Ok(())
        }
    }

    #[derive(Clone)]
    struct Metrics {
        last_eth_block_w_blob: IntGauge,
        last_finalization_time: IntGauge,
        last_finalization_interval: IntGauge,
    }

    impl<L1, Db, Clock> RegistersMetrics for StateListener<L1, Db, Clock> {
        fn metrics(&self) -> Vec<Box<dyn Collector>> {
            vec![
                Box::new(self.metrics.last_eth_block_w_blob.clone()),
                Box::new(self.metrics.last_finalization_time.clone()),
                Box::new(self.metrics.last_finalization_interval.clone()),
            ]
        }
    }

    impl Metrics {
        fn new(last_finalization_time: IntGauge) -> Self {
            let last_eth_block_w_blob = IntGauge::with_opts(Opts::new(
                "last_eth_block_w_blob",
                "The height of the latest Ethereum block used for state submission.",
            ))
            .expect("last_eth_block_w_blob metric to be correctly configured");

            let last_finalization_interval = IntGauge::new(
                "seconds_from_earliest_submission_to_finalization",
                "The number of seconds from the earliest submission to finalization",
            )
            .expect(
                "seconds_from_earliest_submission_to_finalization gauge to be correctly configured",
            );

            Self {
                last_eth_block_w_blob,
                last_finalization_time,
                last_finalization_interval,
            }
        }
    }
}

pub mod port {
    use crate::{
        Result,
        types::{DateTime, L1Tx, TransactionCostUpdate, TransactionState, Utc},
    };

    pub mod l1 {
        use crate::{
            Result,
            types::{L1Height, TransactionResponse},
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
            async fn note_tx_failure(&self, reason: &str) -> Result<()>;
        }
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Sync {
        async fn get_non_finalized_txs(&self) -> Result<Vec<L1Tx>>;
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
}
