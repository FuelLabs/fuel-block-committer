pub mod service {
    use std::{num::NonZeroUsize, time::Duration};

    use crate::{
        types::{storage::BundleFragment, CollectNonEmpty, DateTime, L1Tx, NonEmpty, Utc},
        Result, Runner,
    };
    use itertools::Itertools;
    use metrics::{
        prometheus::{core::Collector, IntGauge, Opts},
        RegistersMetrics,
    };
    use tracing::info;

    // src/config.rs
    #[derive(Debug, Clone)]
    pub struct Config {
        /// The lookback window in blocks to determine the starting height.
        pub lookback_window: u32,
        pub fragment_accumulation_timeout: Duration,
        pub fragments_to_accumulate: NonZeroUsize,
        pub gas_bump_timeout: Duration,
    }

    #[cfg(feature = "test-helpers")]
    impl Default for Config {
        fn default() -> Self {
            Self {
                lookback_window: 1000,
                fragment_accumulation_timeout: Duration::from_secs(0),
                fragments_to_accumulate: 1.try_into().unwrap(),
                gas_bump_timeout: Duration::from_secs(300),
            }
        }
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait SendOrWaitDecider {
        async fn should_send_blob_tx(
            &self,
            num_blobs: u32,
            num_l2_blocks_behind: u32,
            at_l1_height: u64,
        ) -> Result<bool>;
    }

    struct Metrics {
        num_l2_blocks_behind: IntGauge,
    }

    impl Default for Metrics {
        fn default() -> Self {
            let num_l2_blocks_behind = IntGauge::with_opts(Opts::new(
                "num_l2_blocks_behind",
                "How many L2 blocks have been produced since the starting height of the oldest bundle we're committing",
            )).expect("metric config to be correct");

            Self {
                num_l2_blocks_behind,
            }
        }
    }

    impl<L1, FuelApi, Db, Clock, D> RegistersMetrics for StateCommitter<L1, FuelApi, Db, Clock, D> {
        fn metrics(&self) -> Vec<Box<dyn Collector>> {
            vec![Box::new(self.metrics.num_l2_blocks_behind.clone())]
        }
    }

    /// The `StateCommitter` is responsible for committing state fragments to L1.
    pub struct StateCommitter<L1, FuelApi, Db, Clock, D> {
        l1_adapter: L1,
        fuel_api: FuelApi,
        storage: Db,
        config: Config,
        clock: Clock,
        startup_time: DateTime<Utc>,
        decider: D,
        metrics: Metrics,
    }

    impl<L1, FuelApi, Db, Clock, Decider> StateCommitter<L1, FuelApi, Db, Clock, Decider>
    where
        Clock: crate::state_committer::port::Clock,
    {
        /// Creates a new `StateCommitter`.
        pub fn new(
            l1_adapter: L1,
            fuel_api: FuelApi,
            storage: Db,
            config: Config,
            clock: Clock,
            decider: Decider,
        ) -> Self {
            let startup_time = clock.now();

            Self {
                l1_adapter,
                fuel_api,
                storage,
                config,
                clock,
                startup_time,
                decider,
                metrics: Default::default(),
            }
        }
    }

    impl<L1, FuelApi, Db, Clock, Decider> StateCommitter<L1, FuelApi, Db, Clock, Decider>
    where
        L1: crate::state_committer::port::l1::Api + Send + Sync,
        FuelApi: crate::state_committer::port::fuel::Api,
        Db: crate::state_committer::port::Storage,
        Clock: crate::state_committer::port::Clock,
        Decider: SendOrWaitDecider,
    {
        async fn get_reference_time(&self) -> Result<DateTime<Utc>> {
            Ok(self
                .storage
                .last_time_a_fragment_was_finalized()
                .await?
                .unwrap_or(self.startup_time))
        }

        async fn is_timeout_expired(&self) -> Result<bool> {
            let reference_time = self.get_reference_time().await?;
            let elapsed = self.clock.now() - reference_time;
            let std_elapsed = elapsed
                .to_std()
                .map_err(|e| crate::Error::Other(format!("Failed to convert time: {}", e)))?;
            Ok(std_elapsed >= self.config.fragment_accumulation_timeout)
        }

        async fn should_send_tx(&self, fragments: &NonEmpty<BundleFragment>) -> Result<bool> {
            let l1_height = self.l1_adapter.current_height().await?;
            let l2_height = self.fuel_api.latest_height().await?;

            let oldest_l2_block_in_fragments = fragments
                .maximum_by_key(|b| b.oldest_block_in_bundle)
                .oldest_block_in_bundle;

            let num_l2_blocks_behind = l2_height.saturating_sub(oldest_l2_block_in_fragments);

            self.metrics
                .num_l2_blocks_behind
                .set(num_l2_blocks_behind as i64);

            self.decider
                .should_send_blob_tx(
                    u32::try_from(fragments.len()).expect("not to send more than u32::MAX blobs"),
                    num_l2_blocks_behind,
                    l1_height,
                )
                .await
        }

        async fn submit_fragments(
            &self,
            fragments: NonEmpty<BundleFragment>,
            previous_tx: Option<L1Tx>,
        ) -> Result<()> {
            if !self.should_send_tx(&fragments).await? {
                info!("decided against sending fragments due to high fees");
                return Ok(());
            }
            info!("about to send at most {} fragments", fragments.len());

            let data = fragments.clone().map(|f| f.fragment);

            match self
                .l1_adapter
                .submit_state_fragments(data, previous_tx)
                .await
            {
                Ok((submitted_tx, submitted_fragments)) => {
                    let fragment_ids = fragments
                        .iter()
                        .map(|f| f.id)
                        .take(submitted_fragments.num_fragments.get())
                        .collect_nonempty()
                        .expect("non-empty vec");

                    let ids = fragment_ids
                        .iter()
                        .map(|id| id.as_u32().to_string())
                        .join(", ");

                    let tx_hash = submitted_tx.hash;
                    self.storage
                        .record_pending_tx(submitted_tx, fragment_ids, self.clock.now())
                        .await?;

                    tracing::info!("Submitted fragments {ids} with tx {}", hex::encode(tx_hash));
                    Ok(())
                }
                Err(e) => {
                    let ids = fragments
                        .iter()
                        .map(|f| f.id.as_u32().to_string())
                        .join(", ");

                    tracing::error!("Failed to submit fragments {ids}: {e}");

                    Err(e)
                }
            }
        }

        async fn latest_pending_transaction(&self) -> Result<Option<L1Tx>> {
            let tx = self.storage.get_latest_pending_txs().await?;
            Ok(tx)
        }

        async fn next_fragments_to_submit(&self) -> Result<Option<NonEmpty<BundleFragment>>> {
            let latest_height = self.fuel_api.latest_height().await?;
            let starting_height = latest_height.saturating_sub(self.config.lookback_window);

            // although we shouldn't know at this layer how many fragments the L1 can accept, we ignore
            // this for now and put the eth value of max blobs per block (6).
            let existing_fragments = self
                .storage
                .oldest_nonfinalized_fragments(starting_height, 6)
                .await?;

            Ok(NonEmpty::collect(existing_fragments))
        }

        async fn should_submit_fragments(&self, fragment_count: NonZeroUsize) -> Result<bool> {
            if fragment_count >= self.config.fragments_to_accumulate {
                return Ok(true);
            }
            info!(
                "have only {} out of the target {} fragments per tx",
                fragment_count, self.config.fragments_to_accumulate
            );

            let expired = self.is_timeout_expired().await?;
            if expired {
                info!(
                    "fragment accumulation timeout expired, proceeding with {} fragments",
                    fragment_count
                );
            }

            Ok(expired)
        }

        async fn submit_fragments_if_ready(&self) -> Result<()> {
            if let Some(fragments) = self.next_fragments_to_submit().await? {
                if self
                    .should_submit_fragments(fragments.len_nonzero())
                    .await?
                {
                    self.submit_fragments(fragments, None).await?;
                }
            }
            Ok(())
        }

        fn elapsed_since_tx_submitted(&self, tx: &L1Tx) -> Result<Duration> {
            let created_at = tx.created_at.expect("tx to have timestamp");

            self.clock.elapsed(created_at)
        }

        async fn fragments_submitted_by_tx(
            &self,
            tx_hash: [u8; 32],
        ) -> Result<NonEmpty<BundleFragment>> {
            let fragments = self.storage.fragments_submitted_by_tx(tx_hash).await?;

            match NonEmpty::collect(fragments) {
                Some(fragments) => Ok(fragments),
                None => Err(crate::Error::Other(format!(
                    "no fragments found for previously submitted tx {}",
                    hex::encode(tx_hash)
                ))),
            }
        }

        async fn resubmit_fragments_if_stalled(&self) -> Result<()> {
            let Some(previous_tx) = self.latest_pending_transaction().await? else {
                return Ok(());
            };

            let elapsed = self.elapsed_since_tx_submitted(&previous_tx)?;

            if elapsed >= self.config.gas_bump_timeout {
                info!(
                    "replacing tx {} because it was pending for {}s",
                    hex::encode(previous_tx.hash),
                    elapsed.as_secs()
                );

                let fragments = self.fragments_submitted_by_tx(previous_tx.hash).await?;
                self.submit_fragments(fragments, Some(previous_tx)).await?;
            }

            Ok(())
        }
    }

    impl<L1, FuelApi, Db, Clock, Decider> Runner for StateCommitter<L1, FuelApi, Db, Clock, Decider>
    where
        L1: crate::state_committer::port::l1::Api + Send + Sync,
        FuelApi: crate::state_committer::port::fuel::Api + Send + Sync,
        Db: crate::state_committer::port::Storage + Clone + Send + Sync,
        Clock: crate::state_committer::port::Clock + Send + Sync,
        Decider: SendOrWaitDecider + Send + Sync,
    {
        async fn run(&mut self) -> Result<()> {
            if self.storage.has_nonfinalized_txs().await? {
                self.resubmit_fragments_if_stalled().await?
            } else {
                self.submit_fragments_if_ready().await?
            };

            Ok(())
        }
    }
}

pub mod port {
    use nonempty::NonEmpty;

    use crate::{
        types::{storage::BundleFragment, DateTime, L1Tx, NonNegative, Utc},
        Error, Result,
    };

    pub mod l1 {

        use nonempty::NonEmpty;

        use crate::{
            types::{BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Tx},
            Result,
        };
        #[allow(async_fn_in_trait)]
        #[trait_variant::make(Send)]
        #[cfg_attr(feature = "test-helpers", mockall::automock)]
        pub trait Contract: Send + Sync {
            async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;
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
            ) -> Result<(L1Tx, FragmentsSubmitted)>;
        }
    }

    pub mod fuel {
        use crate::Result;
        pub use fuel_core_client::client::types::block::Block as FuelBlock;

        #[allow(async_fn_in_trait)]
        #[trait_variant::make(Send)]
        #[cfg_attr(feature = "test-helpers", mockall::automock)]
        pub trait Api: Send + Sync {
            async fn latest_height(&self) -> Result<u32>;
        }
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Send + Sync {
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

        async fn fragments_submitted_by_tx(&self, tx_hash: [u8; 32])
            -> Result<Vec<BundleFragment>>;
        async fn get_latest_pending_txs(&self) -> Result<Option<L1Tx>>;
    }

    pub trait Clock {
        fn now(&self) -> DateTime<Utc>;
        fn elapsed(&self, since: DateTime<Utc>) -> Result<std::time::Duration> {
            self.now()
                .signed_duration_since(since)
                .to_std()
                .map_err(|e| Error::Other(format!("failed to convert time: {}", e)))
        }
    }
}
