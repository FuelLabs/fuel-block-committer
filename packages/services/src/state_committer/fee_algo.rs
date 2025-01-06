use std::{
    num::{NonZeroU32, NonZeroU64},
    ops::RangeInclusive,
};

use tracing::info;

use crate::{
    fee_metrics_tracker::{self},
    Error, Result,
};

impl<P> SmaFeeAlgo<P>
where
    P: crate::fees::Api + Send + Sync,
{
    pub async fn fees_acceptable(
        &self,
        num_blobs: u32,
        num_l2_blocks_behind: u32,
        at_l1_height: u64,
    ) -> Result<bool> {
        if self.too_far_behind(num_l2_blocks_behind) {
            info!(
                "Sending because we've fallen behind by {} which is more than the configured maximum of {}",
                num_l2_blocks_behind, self.config.fee_thresholds.max_l2_blocks_behind
            );
            return Ok(true);
        }

        // opted out of validating that num_blobs <= 6, it's not this fn's problem if the caller
        // wants to send more than 6 blobs
        let last_n_blocks = |n| last_n_blocks(at_l1_height, n);

        let short_term_sma = self
            .fee_provider
            .fees(last_n_blocks(self.config.sma_periods.short))
            .await?
            .mean();

        let long_term_sma = self
            .fee_provider
            .fees(last_n_blocks(self.config.sma_periods.long))
            .await?
            .mean();

        let short_term_tx_fee =
            fee_metrics_tracker::service::calculate_blob_tx_fee(num_blobs, &short_term_sma);

        if self.fee_always_acceptable(short_term_tx_fee) {
            info!(
                "Sending because: short term price {short_term_tx_fee} is deemed always acceptable since it is <= {}",
                self.config.fee_thresholds.always_acceptable_fee
            );
            return Ok(true);
        }

        let long_term_tx_fee =
            fee_metrics_tracker::service::calculate_blob_tx_fee(num_blobs, &long_term_sma);
        let max_upper_tx_fee = calculate_max_upper_fee(
            &self.config.fee_thresholds,
            long_term_tx_fee,
            num_l2_blocks_behind,
        );

        info!( "short_term_tx_fee: {short_term_tx_fee}, long_term_tx_fee: {long_term_tx_fee}, max_upper_tx_fee: {max_upper_tx_fee}");

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

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub sma_periods: SmaPeriods,
    pub fee_thresholds: FeeThresholds,
}

#[cfg(feature = "test-helpers")]
impl Default for Config {
    fn default() -> Self {
        Self {
            sma_periods: SmaPeriods {
                short: 1.try_into().expect("not zero"),
                long: 2.try_into().expect("not zero"),
            },
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: u128::MAX,
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FeeMultiplierRange {
    starting_multiplier: f64,
    ending_multiplier: f64,
}

#[cfg(feature = "test-helpers")]
impl Default for FeeMultiplierRange {
    fn default() -> Self {
        Self {
            starting_multiplier: 1.,
            ending_multiplier: 1.,
        }
    }
}

impl FeeMultiplierRange {
    pub fn new(starting_multiplier: f64, ending_multiplier: f64) -> Result<Self> {
        if starting_multiplier <= 0.0 {
            return Err(Error::Other(format!(
                "Invalid starting multiplier value: {starting_multiplier}",
            )));
        }
        if ending_multiplier <= 0.0 {
            return Err(Error::Other(format!(
                "Invalid ending multiplier value: {ending_multiplier}",
            )));
        }

        if starting_multiplier > ending_multiplier {
            return Err(Error::Other(format!(
                "Starting multiplier {starting_multiplier} is greater than ending multiplier {ending_multiplier}",
            )));
        }

        Ok(Self {
            starting_multiplier,
            ending_multiplier,
        })
    }

    #[cfg(feature = "test-helpers")]
    pub const fn new_unchecked(starting_multiplier: f64, ending_multiplier: f64) -> Self {
        Self {
            starting_multiplier,
            ending_multiplier,
        }
    }

    pub fn start_ppm(&self) -> u128 {
        to_ppm(self.starting_multiplier)
    }

    pub fn end_ppm(&self) -> u128 {
        to_ppm(self.ending_multiplier)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FeeThresholds {
    pub max_l2_blocks_behind: NonZeroU32,
    pub multiplier_range: FeeMultiplierRange,
    pub always_acceptable_fee: u128,
}

#[derive(Debug, Clone, Copy)]
pub struct SmaPeriods {
    pub short: NonZeroU64,
    pub long: NonZeroU64,
}

#[cfg(feature = "test-helpers")]
impl Default for FeeThresholds {
    fn default() -> Self {
        Self {
            max_l2_blocks_behind: NonZeroU32::new(u32::MAX).unwrap(),
            multiplier_range: FeeMultiplierRange::default(),
            always_acceptable_fee: u128::MAX,
        }
    }
}

#[derive(Clone)]
pub struct SmaFeeAlgo<P> {
    fee_provider: P,
    config: Config,
}

impl<P> SmaFeeAlgo<P> {
    pub const fn new(fee_provider: P, config: Config) -> Self {
        Self {
            fee_provider,
            config,
        }
    }

    const fn too_far_behind(&self, num_l2_blocks_behind: u32) -> bool {
        num_l2_blocks_behind >= self.config.fee_thresholds.max_l2_blocks_behind.get()
    }

    const fn fee_always_acceptable(&self, short_term_tx_fee: u128) -> bool {
        short_term_tx_fee <= self.config.fee_thresholds.always_acceptable_fee
    }
}

const fn last_n_blocks(current_block: u64, n: NonZeroU64) -> RangeInclusive<u64> {
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
        "blocks_behind ({blocks_behind}) should not exceed max_blocks_behind ({max_blocks_behind}), it should have been handled earlier",
    );

    let multiplier_ppm = {
        let start_multiplier_ppm = fee_thresholds.multiplier_range.start_ppm();
        let end_multiplier_ppm = fee_thresholds.multiplier_range.end_ppm();

        // Linear interpolation: start + (end - start) * (blocks_behind / max_blocks_behind)
        let delta_ppm = end_multiplier_ppm.saturating_sub(start_multiplier_ppm);
        let increase_ppm = delta_ppm
            .saturating_mul(blocks_behind)
            .saturating_div(max_blocks_behind);

        let multiplier_ppm = start_multiplier_ppm.saturating_add(increase_ppm);
        // safeguard against surpassing end_multiplier
        multiplier_ppm.min(end_multiplier_ppm)
    };

    let max_fee = from_ppm(fee.saturating_mul(multiplier_ppm));
    {
        let multiplier_perc = multiplier_ppm as f64 / 1_000_000.;
        info!( "{blocks_behind}/{max_blocks_behind} blocks behind -> long term fee({fee}) * multiplier({multiplier_perc}) = max_fee({max_fee})");
    }

    max_fee
}

const fn to_ppm(val: f64) -> u128 {
    (val * 1_000_000.) as u128
}

const fn from_ppm(val: u128) -> u128 {
    val.saturating_div(1_000_000)
}

#[cfg(test)]
mod tests {
    use super::{Config, SmaPeriods};

    mod decision_making {
        pub use test_case::test_case;

        use super::{Config, SmaPeriods};
        use crate::{
            fees::{testing::PreconfiguredFeeApi, Api, Fees},
            state_committer::{
                fee_algo::{FeeMultiplierRange, SmaFeeAlgo},
                FeeThresholds,
            },
        };

        struct Setup {
            old_fees: Fees,
            new_fees: Fees,
            num_blobs: u32,
            num_l2_blocks_behind: u32,
            should_send: bool,
        }

        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
            new_fees: Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000},
            num_blobs: 6,
            num_l2_blocks_behind: 0,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            },
        };
        "Should send because all short-term fees are lower than long-term"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000},
            new_fees: Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
            num_blobs: 6,
            num_l2_blocks_behind: 0,
            should_send: false,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            },
        };
        "Should not send because all short-term fees are higher than long-term"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 3000, reward: 3000, base_fee_per_blob_gas: 3000},
            new_fees: Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
            num_blobs: 6,
            num_l2_blocks_behind: 0,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                always_acceptable_fee: (21_000 * (5000 + 5000)) + (6 * 131_072 * 5000) + 1,
                max_l2_blocks_behind: 100.try_into().unwrap(),
                ..Default::default()
            }
        };
        "Should send since short-term fee less than always_acceptable_fee"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000},
            new_fees: Fees { base_fee_per_gas: 1500, reward: 10000, base_fee_per_blob_gas: 1000},
            num_blobs: 5,
            num_l2_blocks_behind: 0,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Should send because short-term base_fee_per_gas is lower"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000},
            new_fees: Fees { base_fee_per_gas: 2500, reward: 10000, base_fee_per_blob_gas: 1000},
            num_blobs: 5,
            num_l2_blocks_behind: 0,
            should_send: false,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Should not send because short-term base_fee_per_gas is higher"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1000},
            new_fees: Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 900},
            num_blobs: 5,
            num_l2_blocks_behind: 0,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Should send because short-term base_fee_per_blob_gas is lower"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1000},
            new_fees: Fees { base_fee_per_gas: 2000, reward: 3000, base_fee_per_blob_gas: 1100},
            num_blobs: 5,
            num_l2_blocks_behind: 0,
            should_send: false,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Should not send because short-term base_fee_per_blob_gas is higher"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000},
            new_fees: Fees { base_fee_per_gas: 2000, reward: 9000, base_fee_per_blob_gas: 1000},
            num_blobs: 5,
            num_l2_blocks_behind: 0,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Should send because short-term reward is lower"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 2000, reward: 10000, base_fee_per_blob_gas: 1000},
            new_fees: Fees { base_fee_per_gas: 2000, reward: 11000, base_fee_per_blob_gas: 1000},
            num_blobs: 5,
            num_l2_blocks_behind: 0,
            should_send: false,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Should not send because short-term reward is higher"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 4000, reward: 8000, base_fee_per_blob_gas: 4000},
            new_fees: Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 3500},
            num_blobs: 6,
            num_l2_blocks_behind: 0,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Should send because multiple short-term fees are lower"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
            new_fees: Fees { base_fee_per_gas: 5000, reward: 5000, base_fee_per_blob_gas: 5000},
            num_blobs: 6,
            num_l2_blocks_behind: 0,
            should_send: false,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Should not send because all fees are identical and no tolerance"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000},
            new_fees: Fees { base_fee_per_gas: 2500, reward: 5500, base_fee_per_blob_gas: 5000},
            num_blobs: 0,
            num_l2_blocks_behind: 0,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Zero blobs: short-term base_fee_per_gas and reward are lower, send"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000},
            new_fees: Fees { base_fee_per_gas: 3000, reward: 7000, base_fee_per_blob_gas: 5000},
            num_blobs: 0,
            num_l2_blocks_behind: 0,
            should_send: false,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Zero blobs: short-term reward is higher, don't send"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 3000, reward: 6000, base_fee_per_blob_gas: 5000},
            new_fees: Fees { base_fee_per_gas: 2000, reward: 6000, base_fee_per_blob_gas: 50_000_000},
            num_blobs: 0,
            num_l2_blocks_behind: 0,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                always_acceptable_fee: 0,
                ..Default::default()
            }
        };
        "Zero blobs: ignore blob fee, short-term base_fee_per_gas is lower, send"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 6000, reward: 1, base_fee_per_blob_gas: 6000},
            new_fees: Fees { base_fee_per_gas: 7000, reward: 1, base_fee_per_blob_gas: 7000},
            num_blobs: 1,
            num_l2_blocks_behind: 0,
            should_send: false,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                multiplier_range: FeeMultiplierRange::new_unchecked(1.0, 1.2),
                always_acceptable_fee: 0,
            },
        };
        "Early: short-term expensive, not send"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 6000, reward: 1, base_fee_per_blob_gas: 6000},
            new_fees: Fees { base_fee_per_gas: 7000, reward: 1, base_fee_per_blob_gas: 7000},
            num_blobs: 1,
            num_l2_blocks_behind: 100,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                multiplier_range: FeeMultiplierRange::new_unchecked(1.0, 1.2),
                always_acceptable_fee: 0,
            }
        };
        "Later: after max wait, send regardless"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 6000, reward: 1, base_fee_per_blob_gas: 6000},
            new_fees: Fees { base_fee_per_gas: 7000, reward: 1, base_fee_per_blob_gas: 7000},
            num_blobs: 1,
            num_l2_blocks_behind: 80,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                multiplier_range: FeeMultiplierRange::new_unchecked(1.0, 1.2),
                always_acceptable_fee: 0,
            },
        };
        "Mid-wait: increased tolerance allows acceptance"
    )]
        #[test_case(
        Setup {
            old_fees: Fees { base_fee_per_gas: 100_000, reward: 1, base_fee_per_blob_gas: 100_000},
            new_fees: Fees { base_fee_per_gas: 2_000_000, reward: 1_000_000, base_fee_per_blob_gas: 20_000_000},
            num_blobs: 1,
            num_l2_blocks_behind: 0,
            should_send: true,
        },
        Config {
            sma_periods: SmaPeriods { short: 2.try_into().unwrap(), long: 6.try_into().unwrap()},
            fee_thresholds: FeeThresholds {
                max_l2_blocks_behind: 100.try_into().unwrap(),
                multiplier_range: FeeMultiplierRange::new_unchecked(1.0, 1.2),
                always_acceptable_fee: 2_700_000_000_000,
            },
        };
        "Always acceptable fee triggers immediate send"
    )]
        #[tokio::test]
        async fn parameterized_send_or_wait_tests(
            Setup {
                old_fees,
                new_fees,
                num_blobs,
                num_l2_blocks_behind,
                should_send,
            }: Setup,
            config: Config,
        ) {
            let fees = generate_fees(config.sma_periods, old_fees, new_fees);
            let api = PreconfiguredFeeApi::new(fees);
            let current_block_height = api.current_height().await.unwrap();

            let sut = SmaFeeAlgo::new(api, config);

            let decision = sut
                .fees_acceptable(num_blobs, num_l2_blocks_behind, current_block_height)
                .await
                .unwrap();

            assert_eq!(
            decision, should_send,
            "For num_blobs={num_blobs}, num_l2_blocks_behind={num_l2_blocks_behind}, config={config:?}: Expected decision: {should_send}, got: {decision}",
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
    }

    mod upper_fee {
        pub use test_case::test_case;

        use crate::state_committer::{
            fee_algo::{calculate_max_upper_fee, FeeMultiplierRange},
            FeeThresholds,
        };
        struct Setup {
            fee: u128,
            num_l2_blocks_behind: u32,
            expected_max_upper_fee: u128,
        }

        #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            always_acceptable_fee: 0,
            ..Default::default()
        },
        Setup {
            fee: 1000,
            num_l2_blocks_behind: 0,
            expected_max_upper_fee: 1000
        };
        "No blocks behind, multiplier should be 1.0"
    )]
        #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            multiplier_range: FeeMultiplierRange::new_unchecked(1.0, 1.05),
            always_acceptable_fee: 0,
        },
        Setup {
            fee: 2000,
            num_l2_blocks_behind: 50,
            expected_max_upper_fee: 2050
        };
        "Half blocks behind with multiplier increase"
    )]
        #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            multiplier_range: FeeMultiplierRange::new_unchecked(0.95, 1.0),
            always_acceptable_fee: 0,
        },
        Setup {
            fee: 800,
            num_l2_blocks_behind: 50,
            expected_max_upper_fee: 780
        };
        "Start multiplier less than 1, no premium"
    )]
        #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            multiplier_range: FeeMultiplierRange::new_unchecked(1.0, 1.3),
            always_acceptable_fee: 0,
        },
        Setup {
            fee: 1000,
            num_l2_blocks_behind: 50,
            expected_max_upper_fee: 1150
        }; // 1.0 + (1.3 - 1.0) * (50/100) = 1.15 -> 1000 * 1.15 = 1150
        "End multiplier greater than 1, with premium"
    )]
        #[test_case(
        FeeThresholds {
            max_l2_blocks_behind: 100.try_into().unwrap(),
            multiplier_range: FeeMultiplierRange::new_unchecked(1.0, 1.2),
            always_acceptable_fee: 0,
        },
        Setup {
            fee: 10_000,
            num_l2_blocks_behind: 99,
            expected_max_upper_fee: 11_980
        };
        "High fee with premium"
    )]
        fn test_calculate_max_upper_fee(
            fee_thresholds: FeeThresholds,
            Setup {
                fee,
                num_l2_blocks_behind,
                expected_max_upper_fee,
            }: Setup,
        ) {
            let max_upper_fee = calculate_max_upper_fee(&fee_thresholds, fee, num_l2_blocks_behind);

            assert_eq!(
                max_upper_fee, expected_max_upper_fee,
                "Expected max_upper_fee to be {}, but got {}",
                expected_max_upper_fee, max_upper_fee
            );
        }
    }
}
