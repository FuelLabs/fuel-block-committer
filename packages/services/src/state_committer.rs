pub mod service {
    use std::{
        cmp::min,
        num::{NonZeroU32, NonZeroU64, NonZeroUsize},
        ops::RangeInclusive,
        time::Duration,
    };

    use itertools::Itertools;
    use metrics::{
        prometheus::{core::Collector, IntGauge, Opts},
        RegistersMetrics,
    };
    use tracing::info;

    use crate::{
        historical_fees::{
            self,
            service::{HistoricalFees, SmaPeriods},
        },
        types::{storage::BundleFragment, CollectNonEmpty, DateTime, L1Tx, NonEmpty, Utc},
        Error, Result, Runner,
    };

    // src/config.rs
    #[derive(Debug, Clone)]
    pub struct Config {
        /// The lookback window in blocks to determine the starting height.
        pub lookback_window: u32,
        pub fragment_accumulation_timeout: Duration,
        pub fragments_to_accumulate: NonZeroUsize,
        pub gas_bump_timeout: Duration,
        pub fee_algo: AlgoConfig,
    }

    #[cfg(feature = "test-helpers")]
    impl Default for Config {
        fn default() -> Self {
            Self {
                lookback_window: 1000,
                fragment_accumulation_timeout: Duration::from_secs(0),
                fragments_to_accumulate: 1.try_into().unwrap(),
                gas_bump_timeout: Duration::from_secs(300),
                fee_algo: Default::default(),
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct AlgoConfig {
        pub sma_periods: SmaPeriods,
        pub fee_thresholds: FeeThresholds,
    }

    #[cfg(feature = "test-helpers")]
    impl Default for AlgoConfig {
        fn default() -> Self {
            Self {
                sma_periods: SmaPeriods {
                    short: 1.try_into().expect("not zero"),
                    long: 2.try_into().expect("not zero"),
                },
                fee_thresholds: FeeThresholds {
                    max_l2_blocks_behind: 100.try_into().unwrap(),
                    ..FeeThresholds::default()
                },
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct FeeThresholds {
        pub max_l2_blocks_behind: NonZeroU32,
        pub start_discount_percentage: Percentage,
        pub end_premium_percentage: Percentage,
        pub always_acceptable_fee: u128,
    }

    #[cfg(feature = "test-helpers")]
    impl Default for FeeThresholds {
        fn default() -> Self {
            Self {
                max_l2_blocks_behind: NonZeroU32::MAX,
                start_discount_percentage: Percentage::ZERO,
                end_premium_percentage: Percentage::ZERO,
                always_acceptable_fee: u128::MAX,
            }
        }
    }

    #[derive(Default, Copy, Clone, Debug, PartialEq)]
    pub struct Percentage(f64);

    impl TryFrom<f64> for Percentage {
        type Error = Error;

        fn try_from(value: f64) -> std::result::Result<Self, Self::Error> {
            if value < 0. {
                return Err(Error::Other(format!("Invalid percentage value {value}")));
            }

            Ok(Self(value))
        }
    }

    impl From<Percentage> for f64 {
        fn from(value: Percentage) -> Self {
            value.0
        }
    }

    impl Percentage {
        pub const ZERO: Self = Percentage(0.);
        pub const PPM: u128 = 1_000_000;

        pub fn ppm(&self) -> u128 {
            (self.0 * 1_000_000.) as u128
        }
    }

    pub struct SmaFeeAlgo<P> {
        historical_fees: HistoricalFees<P>,
        config: AlgoConfig,
    }

    impl<P> SmaFeeAlgo<P> {
        pub fn new(historical_fees: HistoricalFees<P>, config: AlgoConfig) -> Self {
            Self {
                historical_fees,
                config,
            }
        }

        fn too_far_behind(&self, num_l2_blocks_behind: u32) -> bool {
            num_l2_blocks_behind >= self.config.fee_thresholds.max_l2_blocks_behind.get()
        }

        fn fee_always_acceptable(&self, short_term_tx_fee: u128) -> bool {
            short_term_tx_fee <= self.config.fee_thresholds.always_acceptable_fee
        }

        fn last_n_blocks(current_block: u64, n: NonZeroU64) -> RangeInclusive<u64> {
            current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
        }

        fn calculate_max_upper_fee(
            fee_thresholds: &FeeThresholds,
            fee: u128,
            num_l2_blocks_behind: u32,
        ) -> u128 {
            let max_blocks_behind = u128::from(fee_thresholds.max_l2_blocks_behind.get());
            let blocks_behind = u128::from(num_l2_blocks_behind);

            debug_assert!(
            blocks_behind <= max_blocks_behind,
            "blocks_behind ({}) should not exceed max_blocks_behind ({}), it should have been handled earlier",
            blocks_behind,
            max_blocks_behind
        );

            let start_discount_ppm = min(
                fee_thresholds.start_discount_percentage.ppm(),
                Percentage::PPM,
            );
            let end_premium_ppm = fee_thresholds.end_premium_percentage.ppm();

            // 1. The highest we're initially willing to go: eg. 100% - 20% = 80%
            let base_multiplier = Percentage::PPM.saturating_sub(start_discount_ppm);

            // 2. How late are we: eg. late enough to add 25% to our base multiplier
            let premium_increment = Self::calculate_premium_increment(
                start_discount_ppm,
                end_premium_ppm,
                blocks_behind,
                max_blocks_behind,
            );

            // 3. Total multiplier consist of the base and the premium increment: eg. 80% + 25% = 105%
            let multiplier_ppm = min(
                base_multiplier.saturating_add(premium_increment),
                Percentage::PPM + end_premium_ppm,
            );

            info!("start_discount_ppm: {start_discount_ppm}, end_premium_ppm: {end_premium_ppm}, base_multiplier: {base_multiplier}, premium_increment: {premium_increment}, multiplier_ppm: {multiplier_ppm}");

            // 3. Final fee: eg. 105% of the base fee
            fee.saturating_mul(multiplier_ppm)
                .saturating_div(Percentage::PPM)
        }

        fn calculate_premium_increment(
            start_discount_ppm: u128,
            end_premium_ppm: u128,
            blocks_behind: u128,
            max_blocks_behind: u128,
        ) -> u128 {
            let total_ppm = start_discount_ppm.saturating_add(end_premium_ppm);

            let proportion = if max_blocks_behind == 0 {
                0
            } else {
                blocks_behind
                    .saturating_mul(Percentage::PPM)
                    .saturating_div(max_blocks_behind)
            };

            total_ppm
                .saturating_mul(proportion)
                .saturating_div(Percentage::PPM)
        }
    }

    impl<P> SmaFeeAlgo<P>
    where
        P: historical_fees::port::l1::Api + Send + Sync,
    {
        async fn should_send_blob_tx(
            &self,
            num_blobs: u32,
            num_l2_blocks_behind: u32,
            at_l1_height: u64,
        ) -> Result<bool> {
            if self.too_far_behind(num_l2_blocks_behind) {
                info!("Sending because we've fallen behind by {} which is more than the configured maximum of {}", num_l2_blocks_behind, self.config.fee_thresholds.max_l2_blocks_behind);
                return Ok(true);
            }

            // opted out of validating that num_blobs <= 6, it's not this fn's problem if the caller
            // wants to send more than 6 blobs
            let last_n_blocks = |n| Self::last_n_blocks(at_l1_height, n);

            let short_term_sma = self
                .historical_fees
                .calculate_sma(last_n_blocks(self.config.sma_periods.short))
                .await?;

            let long_term_sma = self
                .historical_fees
                .calculate_sma(last_n_blocks(self.config.sma_periods.long))
                .await?;

            let short_term_tx_fee =
                historical_fees::service::calculate_blob_tx_fee(num_blobs, &short_term_sma);

            if self.fee_always_acceptable(short_term_tx_fee) {
                info!("Sending because: short term price {} is deemed always acceptable since it is <= {}", short_term_tx_fee, self.config.fee_thresholds.always_acceptable_fee);
                return Ok(true);
            }

            let long_term_tx_fee =
                historical_fees::service::calculate_blob_tx_fee(num_blobs, &long_term_sma);
            let max_upper_tx_fee = Self::calculate_max_upper_fee(
                &self.config.fee_thresholds,
                long_term_tx_fee,
                num_l2_blocks_behind,
            );

            info!("short_term_tx_fee: {short_term_tx_fee}, long_term_tx_fee: {long_term_tx_fee}, max_upper_tx_fee: {max_upper_tx_fee}");

            let should_send = short_term_tx_fee < max_upper_tx_fee;

            if should_send {
                info!(
                    "Sending because short term price {} is lower than the max upper fee {}",
                    short_term_tx_fee, max_upper_tx_fee
                );
            } else {
                info!(
                    "Not sending because short term price {} is higher than the max upper fee {}",
                    short_term_tx_fee, max_upper_tx_fee
                );
            }

            Ok(should_send)
        }
    }

    struct Metrics {
        current_height_to_commit: IntGauge,
    }

    impl Default for Metrics {
        fn default() -> Self {
            let current_height_to_commit = IntGauge::with_opts(Opts::new(
                "current_height_to_commit",
                "The starting l2 height of the bundle we're committing/will commit next",
            ))
            .expect("metric config to be correct");

            Self {
                current_height_to_commit,
            }
        }
    }

    impl<L1, FuelApi, Db, Clock, D> RegistersMetrics for StateCommitter<L1, FuelApi, Db, Clock, D> {
        fn metrics(&self) -> Vec<Box<dyn Collector>> {
            vec![Box::new(self.metrics.current_height_to_commit.clone())]
        }
    }

    /// The `StateCommitter` is responsible for committing state fragments to L1.
    pub struct StateCommitter<L1, FuelApi, Db, Clock, FeeProvider> {
        l1_adapter: L1,
        fuel_api: FuelApi,
        storage: Db,
        config: Config,
        clock: Clock,
        startup_time: DateTime<Utc>,
        metrics: Metrics,
        fee_algo: SmaFeeAlgo<FeeProvider>,
    }

    impl<L1, FuelApi, Db, Clock, FeeProvider> StateCommitter<L1, FuelApi, Db, Clock, FeeProvider>
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
            historical_fees: HistoricalFees<FeeProvider>,
        ) -> Self {
            let startup_time = clock.now();

            Self {
                fee_algo: SmaFeeAlgo::new(historical_fees, config.fee_algo),
                l1_adapter,
                fuel_api,
                storage,
                config,
                clock,
                startup_time,
                metrics: Default::default(),
            }
        }
    }

    impl<L1, FuelApi, Db, Clock, FeeProvider> StateCommitter<L1, FuelApi, Db, Clock, FeeProvider>
    where
        L1: crate::state_committer::port::l1::Api + Send + Sync,
        FuelApi: crate::state_committer::port::fuel::Api,
        Db: crate::state_committer::port::Storage,
        Clock: crate::state_committer::port::Clock,
        FeeProvider: historical_fees::port::l1::Api + Sync,
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

        async fn fees_acceptable(&self, fragments: &NonEmpty<BundleFragment>) -> Result<bool> {
            let l1_height = self.l1_adapter.current_height().await?;
            let l2_height = self.fuel_api.latest_height().await?;

            let oldest_l2_block = self.oldest_l2_block_in_fragments(fragments);
            self.update_oldest_block_metric(oldest_l2_block);

            let num_l2_blocks_behind = l2_height.saturating_sub(oldest_l2_block);

            self.fee_algo
                .should_send_blob_tx(
                    u32::try_from(fragments.len()).expect("not to send more than u32::MAX blobs"),
                    num_l2_blocks_behind,
                    l1_height,
                )
                .await
        }

        fn oldest_l2_block_in_fragments(&self, fragments: &NonEmpty<BundleFragment>) -> u32 {
            fragments
                .minimum_by_key(|b| b.oldest_block_in_bundle)
                .oldest_block_in_bundle
        }

        async fn submit_fragments(
            &self,
            fragments: NonEmpty<BundleFragment>,
            previous_tx: Option<L1Tx>,
        ) -> Result<()> {
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

            let fragments = NonEmpty::collect(existing_fragments);

            if let Some(fragments) = fragments.as_ref() {
                // Tracking the metric here as well to get updates more often -- because
                // submit_fragments might not be called
                self.update_oldest_block_metric(self.oldest_l2_block_in_fragments(fragments));
            }

            Ok(fragments)
        }

        fn update_oldest_block_metric(&self, oldest_height: u32) {
            self.metrics
                .current_height_to_commit
                .set(oldest_height as i64);
        }

        async fn should_submit_fragments(
            &self,
            fragments: &NonEmpty<BundleFragment>,
        ) -> Result<bool> {
            let fragment_count = fragments.len_nonzero();

            let expired = || async {
                let expired = self.is_timeout_expired().await?;
                if expired {
                    info!(
                        "fragment accumulation timeout expired, available {}/{} fragments",
                        fragment_count, self.config.fragments_to_accumulate
                    );
                }
                Result::Ok(expired)
            };

            let enough_fragments = || {
                let enough_fragments = fragment_count >= self.config.fragments_to_accumulate;
                if !enough_fragments {
                    info!(
                        "not enough fragments {}/{}",
                        fragment_count, self.config.fragments_to_accumulate
                    );
                };
                enough_fragments
            };

            // wrapped in closures so that we short-circuit *and* reduce redundant logs
            Ok((enough_fragments() || expired().await?) && self.fees_acceptable(fragments).await?)
        }

        async fn submit_fragments_if_ready(&self) -> Result<()> {
            if let Some(fragments) = self.next_fragments_to_submit().await? {
                if self.should_submit_fragments(&fragments).await? {
                    self.submit_fragments(fragments, None).await?;
                }
            } else {
                // TODO: segfault test this
                // if we have no fragments to submit, that means that we're up to date and new
                // blocks haven't been bundled yet
                let current_height_to_commit =
                    if let Some(height) = self.storage.latest_bundled_height().await? {
                        height.saturating_add(1)
                    } else {
                        self.fuel_api
                            .latest_height()
                            .await?
                            .saturating_sub(self.config.lookback_window)
                    };

                self.metrics
                    .current_height_to_commit
                    .set(current_height_to_commit as i64);
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
                if self.fees_acceptable(&fragments).await? {
                    self.submit_fragments(fragments, Some(previous_tx)).await?;
                }
            }

            Ok(())
        }
    }

    impl<L1, FuelApi, Db, Clock, FeeProvider> Runner
        for StateCommitter<L1, FuelApi, Db, Clock, FeeProvider>
    where
        L1: crate::state_committer::port::l1::Api + Send + Sync,
        FuelApi: crate::state_committer::port::fuel::Api + Send + Sync,
        Db: crate::state_committer::port::Storage + Clone + Send + Sync,
        Clock: crate::state_committer::port::Clock + Send + Sync,
        FeeProvider: historical_fees::port::l1::Api + Send + Sync,
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

    #[cfg(test)]
    mod tests {
        use crate::historical_fees::port::l1::testing::PreconfiguredFeeApi;
        use crate::historical_fees::port::l1::{Api, Fees};
        use test_case::test_case;

        use super::*;

        #[test_case(
        // Test Case 1: No blocks behind, no discount or premium
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
        1000,
        0,
        1000;
        "No blocks behind, multiplier should be 100%"
    )]
        #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            start_discount_percentage: 0.20.try_into().unwrap(),
            end_premium_percentage: 0.25.try_into().unwrap(),
            always_acceptable_fee: 0,
        },
        2000,
        50,
        2050;
        "Half blocks behind with discount and premium"
    )]
        #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            start_discount_percentage: 0.25.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
        800,
        50,
        700;
        "Start discount only, no premium"
    )]
        #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            end_premium_percentage: 0.30.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
        1000,
        50,
        1150;
        "End premium only, no discount"
    )]
        #[test_case(
        // Test Case 8: High fee with premium
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            start_discount_percentage: 0.10.try_into().unwrap(),
            end_premium_percentage: 0.20.try_into().unwrap(),
            always_acceptable_fee: 0,
        },
        10_000,
        99,
        11970;
        "High fee with premium"
    )]
        #[test_case(
    FeeThresholds {
        max_l2_blocks_behind: 100.try_into().unwrap(),
        start_discount_percentage: 1.50.try_into().unwrap(), // 150%
        end_premium_percentage: 0.20.try_into().unwrap(),
        always_acceptable_fee: 0,
    },
    1000,
    1,
    12;
    "Discount exceeds 100%, should be capped to 100%"
)]
        fn test_calculate_max_upper_fee(
            fee_thresholds: FeeThresholds,
            fee: u128,
            num_l2_blocks_behind: u32,
            expected_max_upper_fee: u128,
        ) {
            use crate::historical_fees::port::l1::testing::ConstantFeeApi;

            let max_upper_fee = SmaFeeAlgo::<ConstantFeeApi>::calculate_max_upper_fee(
                &fee_thresholds,
                fee,
                num_l2_blocks_behind,
            );

            assert_eq!(
                max_upper_fee, expected_max_upper_fee,
                "Expected max_upper_fee to be {}, but got {}",
                expected_max_upper_fee, max_upper_fee
            );
        }

        fn generate_fees(
            sma_periods: SmaPeriods,
            old_fees: Fees,
            new_fees: Fees,
        ) -> Vec<(u64, Fees)> {
            let older_fees = std::iter::repeat_n(
                old_fees,
                (sma_periods.long.get() - sma_periods.short.get()) as usize,
            );
            let newer_fees = std::iter::repeat_n(new_fees, sma_periods.short.get() as usize);

            older_fees
                .chain(newer_fees)
                .enumerate()
                .map(|(i, f)| (i as u64, f))
                .collect()
        }

        #[test_case(
        Fees { base_fee_per_gas: 5000.try_into().unwrap(), reward: 5000.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap()},
        Fees { base_fee_per_gas: 3000.try_into().unwrap(), reward: 3000.try_into().unwrap(), base_fee_per_blob_gas: 3000.try_into().unwrap()},
        6,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            },
        },
        0, // not behind at all
        true;
        "Should send because all short-term fees are lower than long-term"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 3000.try_into().unwrap(), reward: 3000.try_into().unwrap(), base_fee_per_blob_gas: 3000.try_into().unwrap()},
        Fees { base_fee_per_gas: 5000.try_into().unwrap(), reward: 5000.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap() },
        6,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            },
        },
        0,
        false;
        "Should not send because all short-term fees are higher than long-term"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 3000.try_into().unwrap(), reward: 3000.try_into().unwrap(), base_fee_per_blob_gas: 3000.try_into().unwrap()},
        Fees { base_fee_per_gas: 5000.try_into().unwrap(), reward: 5000.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap()},
        6,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                always_acceptable_fee: (21_000 * (5000 + 5000)) + (6 * 131_072 * 5000) + 1,
                max_l2_blocks_behind: 100.try_into().unwrap(),
                ..Default::default()
            }
        },
        0,
        true;
        "Should send since short-term fee less than always_acceptable_fee"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 10000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap()},
        Fees { base_fee_per_gas: 1500.try_into().unwrap(), reward: 10000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap()},
        5,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Should send because short-term base_fee_per_gas is lower"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 10000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap() },
        Fees { base_fee_per_gas: 2500.try_into().unwrap(), reward: 10000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap()},
        5,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Should not send because short-term base_fee_per_gas is higher"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 3000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap() },
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 3000.try_into().unwrap(), base_fee_per_blob_gas: 900.try_into().unwrap()},
        5,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Should send because short-term base_fee_per_blob_gas is lower"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 3000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap() },
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 3000.try_into().unwrap(), base_fee_per_blob_gas: 1100.try_into().unwrap()},
        5,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Should not send because short-term base_fee_per_blob_gas is higher"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 10000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap()},
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 9000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap()},
        5,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Should send because short-term reward is lower"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 10000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap() },
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 11000.try_into().unwrap(), base_fee_per_blob_gas: 1000.try_into().unwrap() },
        5,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Should not send because short-term reward is higher"
    )]
        #[test_case(
        // Multiple short-term fees are lower
        Fees { base_fee_per_gas: 4000.try_into().unwrap(), reward: 8000.try_into().unwrap(), base_fee_per_blob_gas: 4000.try_into().unwrap() },
        Fees { base_fee_per_gas: 3000.try_into().unwrap(), reward: 7000.try_into().unwrap(), base_fee_per_blob_gas: 3500.try_into().unwrap() },
        6,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Should send because multiple short-term fees are lower"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 5000.try_into().unwrap(), reward: 5000.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap() },
        Fees { base_fee_per_gas: 5000.try_into().unwrap(), reward: 5000.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap() },
        6,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Should not send because all fees are identical and no tolerance"
    )]
        #[test_case(
        // Zero blobs scenario: blob fee differences don't matter
        Fees { base_fee_per_gas: 3000.try_into().unwrap(), reward: 6000.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap() },
        Fees { base_fee_per_gas: 2500.try_into().unwrap(), reward: 5500.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap() },
        0,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Zero blobs: short-term base_fee_per_gas and reward are lower, send"
    )]
        #[test_case(
        // Zero blobs but short-term reward is higher
        Fees { base_fee_per_gas: 3000.try_into().unwrap(), reward: 6000.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap() },
        Fees { base_fee_per_gas: 3000.try_into().unwrap(), reward: 7000.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap() },
        0,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        false;
        "Zero blobs: short-term reward is higher, don't send"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 3000.try_into().unwrap(), reward: 6000.try_into().unwrap(), base_fee_per_blob_gas: 5000.try_into().unwrap() },
        Fees { base_fee_per_gas: 2000.try_into().unwrap(), reward: 6000.try_into().unwrap(), base_fee_per_blob_gas: 50_000_000.try_into().unwrap() },
        0,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        },
        0,
        true;
        "Zero blobs: ignore blob fee, short-term base_fee_per_gas is lower, send"
    )]
        // Initially not send, but as num_l2_blocks_behind increases, acceptance grows.
        #[test_case(
        // Initially short-term fee too high compared to long-term (strict scenario), no send at t=0
    Fees { base_fee_per_gas: 6000.try_into().unwrap(), reward: 1.try_into().unwrap(), base_fee_per_blob_gas: 6000.try_into().unwrap() },
    Fees { base_fee_per_gas: 7000.try_into().unwrap(), reward: 1.try_into().unwrap(), base_fee_per_blob_gas: 7000.try_into().unwrap() },
        1,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                start_discount_percentage: Percentage::try_from(0.20).unwrap(),
                end_premium_percentage: Percentage::try_from(0.20).unwrap(),
                always_acceptable_fee: 0,
            },
        },
        0,
        false;
        "Early: short-term expensive, not send"
    )]
        #[test_case(
        // At max_l2_blocks_behind, send regardless
        Fees { base_fee_per_gas: 6000.try_into().unwrap(), reward: 1.try_into().unwrap(), base_fee_per_blob_gas: 6000.try_into().unwrap() },
        Fees { base_fee_per_gas: 7000.try_into().unwrap(), reward: 1.try_into().unwrap(), base_fee_per_blob_gas: 7000.try_into().unwrap() },
        1,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                start_discount_percentage: 0.20.try_into().unwrap(),
                end_premium_percentage: 0.20.try_into().unwrap(),
                always_acceptable_fee: 0,
            }
        },
        100,
        true;
        "Later: after max wait, send regardless"
    )]
        #[test_case(
        Fees { base_fee_per_gas: 6000.try_into().unwrap(), reward: 1.try_into().unwrap(), base_fee_per_blob_gas: 6000.try_into().unwrap() },
        Fees { base_fee_per_gas: 7000.try_into().unwrap(), reward: 1.try_into().unwrap(), base_fee_per_blob_gas: 7000.try_into().unwrap() },
        1,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                start_discount_percentage: 0.20.try_into().unwrap(),
                end_premium_percentage: 0.20.try_into().unwrap(),
                always_acceptable_fee: 0,
            },
        },
        80,
        true;
        "Mid-wait: increased tolerance allows acceptance"
    )]
        #[test_case(
        // Short-term fee is huge, but always_acceptable_fee is large, so send immediately
        Fees { base_fee_per_gas: 100_000.try_into().unwrap(), reward: 1.try_into().unwrap(), base_fee_per_blob_gas: 100_000.try_into().unwrap() },
        Fees { base_fee_per_gas: 2_000_000.try_into().unwrap(), reward: 1_000_000.try_into().unwrap(), base_fee_per_blob_gas: 20_000_000.try_into().unwrap() },
        1,
        AlgoConfig {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6 .try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                start_discount_percentage: 0.20.try_into().unwrap(),
                end_premium_percentage: 0.20.try_into().unwrap(),
                always_acceptable_fee: 2_700_000_000_000
            },
        },
        0,
        true;
        "Always acceptable fee triggers immediate send"
    )]
        #[tokio::test]
        async fn parameterized_send_or_wait_tests(
            old_fees: Fees,
            new_fees: Fees,
            num_blobs: u32,
            config: AlgoConfig,
            num_l2_blocks_behind: u32,
            expected_decision: bool,
        ) {
            let fees = generate_fees(config.sma_periods, old_fees, new_fees);
            let api = PreconfiguredFeeApi::new(fees);
            let current_block_height = api.current_height().await.unwrap();
            let historical_fees = HistoricalFees::new(api);

            let sut = SmaFeeAlgo::new(historical_fees, config);

            let should_send = sut
                .should_send_blob_tx(num_blobs, num_l2_blocks_behind, current_block_height)
                .await
                .unwrap();

            assert_eq!(
            should_send, expected_decision,
            "For num_blobs={num_blobs}, num_l2_blocks_behind={num_l2_blocks_behind}, config={config:?}: Expected decision: {expected_decision}, got: {should_send}",
        );
        }

        #[tokio::test]
        async fn test_send_when_too_far_behind_and_fee_provider_fails() {
            // given
            let config = AlgoConfig {
                sma_periods: SmaPeriods {
                    short: 2.try_into().unwrap(),
                    long: 6.try_into().unwrap(),
                },
                fee_thresholds: FeeThresholds {
                    max_l2_blocks_behind: 10.try_into().unwrap(),
                    always_acceptable_fee: 0,
                    ..Default::default()
                },
            };

            // having no fees will make the validation in fee analytics fail
            let historical_fees = HistoricalFees::new(PreconfiguredFeeApi::new(vec![]));
            let sut = SmaFeeAlgo::new(historical_fees, config);

            // when
            let should_send = sut
                .should_send_blob_tx(1, 20, 100)
                .await
                .expect("Should send despite fee provider failure");

            // then
            assert!(
                should_send,
                "Should send because too far behind, regardless of fee provider status"
            );
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
        pub use fuel_core_client::client::types::block::Block as FuelBlock;

        use crate::Result;

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
        async fn latest_bundled_height(&self) -> Result<Option<u32>>;
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
