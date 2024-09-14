use std::{collections::HashMap, time::Duration};

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use itertools::Itertools;
use ports::{
    clock::Clock,
    l1::SubmittableFragments,
    storage::{BundleFragment, Storage, ValidatedRange},
    types::{DateTime, NonEmptyVec, Utc},
};

use crate::{Result, Runner};

pub struct GasOptimizingBundler<L1> {
    l1_adapter: L1,
    blocks: Vec<ports::storage::FuelBlock>,
    acceptable_amount_of_blocks: ValidatedRange<usize>,
    best_run: Option<Bundle>,
    next_block_amount: Option<usize>,
}

impl<L1> GasOptimizingBundler<L1> {
    fn new(
        l1_adapter: L1,
        blocks: Vec<ports::storage::FuelBlock>,
        acceptable_amount_of_blocks: ValidatedRange<usize>,
    ) -> Self {
        Self {
            l1_adapter,
            blocks,
            acceptable_amount_of_blocks,
            best_run: None,
            next_block_amount: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub fragments: SubmittableFragments,
    pub block_heights: ValidatedRange<u32>,
    pub optimal: bool,
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait Bundler {
    async fn propose_bundle(&mut self) -> Result<Option<Bundle>>;
}

#[async_trait::async_trait]
pub trait BundlerFactory {
    type Bundler: Bundler + Send;
    async fn build(&self) -> Result<Self::Bundler>;
}

struct GasOptimizingBundlerFactory<L1, Storage> {
    l1: L1,
    storage: Storage,
    acceptable_amount_of_blocks: ValidatedRange<usize>,
}

impl<L1, Storage> GasOptimizingBundlerFactory<L1, Storage> {
    pub fn new(
        l1: L1,
        storage: Storage,
        acceptable_amount_of_blocks: ValidatedRange<usize>,
    ) -> Self {
        Self {
            acceptable_amount_of_blocks,
            l1,
            storage,
        }
    }
}

#[async_trait::async_trait]
impl<L1, Storage> BundlerFactory for GasOptimizingBundlerFactory<L1, Storage>
where
    GasOptimizingBundler<L1>: Bundler,
    Storage: ports::storage::Storage + 'static,
    L1: Send + Sync + 'static + Clone,
{
    type Bundler = GasOptimizingBundler<L1>;
    async fn build(&self) -> Result<Self::Bundler> {
        let max_blocks = self
            .acceptable_amount_of_blocks
            .inner()
            .end
            .saturating_sub(1);
        let blocks = self.storage.lowest_unbundled_blocks(max_blocks).await?;

        Ok(GasOptimizingBundler::new(
            self.l1.clone(),
            blocks,
            self.acceptable_amount_of_blocks.clone(),
        ))
    }
}

#[async_trait::async_trait]
impl<L1> Bundler for GasOptimizingBundler<L1>
where
    L1: ports::l1::Api + Send + Sync,
{
    async fn propose_bundle(&mut self) -> Result<Option<Bundle>> {
        if self.blocks.is_empty() {
            return Ok(None);
        }

        let min_possible_blocks = self
            .acceptable_amount_of_blocks
            .inner()
            .clone()
            .min()
            .unwrap();

        let max_possible_blocks = self
            .acceptable_amount_of_blocks
            .inner()
            .clone()
            .max()
            .unwrap();

        if self.blocks.len() < min_possible_blocks {
            return Ok(None);
        }

        let amount_of_blocks_to_try = self.next_block_amount.unwrap_or(min_possible_blocks);

        let merged_data = self.blocks[..amount_of_blocks_to_try]
            .iter()
            .flat_map(|b| b.data.clone().into_inner())
            .collect::<Vec<_>>();

        let submittable_chunks = self
            .l1_adapter
            .split_into_submittable_fragments(&merged_data.try_into().expect("cannot be empty"))?;

        let fragments = submittable_chunks;

        let (min_height, max_height) = self.blocks.as_slice()[..amount_of_blocks_to_try]
            .iter()
            .map(|b| b.height)
            .minmax()
            .into_option()
            .unwrap();

        let block_heights = (min_height..max_height + 1).try_into().unwrap();

        match &mut self.best_run {
            None => {
                self.best_run = Some(Bundle {
                    fragments,
                    block_heights,
                    optimal: false,
                });
            }
            Some(best_run) => {
                if best_run.fragments.gas_estimation >= fragments.gas_estimation {
                    self.best_run = Some(Bundle {
                        fragments,
                        block_heights,
                        optimal: false,
                    });
                }
            }
        }

        let last_try = amount_of_blocks_to_try == max_possible_blocks;

        let best = self.best_run.as_ref().unwrap().clone();

        self.next_block_amount = Some(amount_of_blocks_to_try.saturating_add(1));

        Ok(Some(Bundle {
            optimal: last_try,
            ..best
        }))
    }
}

pub struct StateCommitter<L1, Storage, Clock, BundlerFactory> {
    l1_adapter: L1,
    storage: Storage,
    clock: Clock,
    component_created_at: DateTime<Utc>,
    bundler_factory: BundlerFactory,
    bundle_generation_config: BundleGenerationConfig,
}

#[derive(Debug, Clone, Copy)]
pub struct BundleGenerationConfig {
    pub stop_optimization_attempts_after: Duration,
}

impl<L1, Storage, C, BF> StateCommitter<L1, Storage, C, BF>
where
    C: Clock,
{
    pub fn new(
        l1_adapter: L1,
        storage: Storage,
        clock: C,
        bundler_factory: BF,
        bundle_generation_config: BundleGenerationConfig,
    ) -> Self {
        let now = clock.now();

        Self {
            l1_adapter,
            storage,
            clock,
            component_created_at: now,
            bundler_factory,
            bundle_generation_config,
        }
    }
}

impl<L1, Db, C, BF> StateCommitter<L1, Db, C, BF>
where
    L1: ports::l1::Api,
    Db: Storage,
    C: Clock,
    BF: BundlerFactory,
{
    async fn bundle_then_fragment(&self) -> crate::Result<Option<NonEmptyVec<BundleFragment>>> {
        // TODO: remove args from build
        let mut bundler = self.bundler_factory.build().await?;

        let start_time = self.clock.now();

        let Bundle {
            fragments,
            block_heights,
            ..
        } = loop {
            if let Some(bundle) = bundler.propose_bundle().await? {
                let now = self.clock.now();

                let elapsed = (now - start_time).to_std().unwrap_or(Duration::ZERO);

                let should_stop_optimizing = elapsed
                    > self
                        .bundle_generation_config
                        .stop_optimization_attempts_after;

                if bundle.optimal || should_stop_optimizing {
                    break bundle;
                }
            } else {
                return Ok(None);
            }
        };

        Ok(Some(
            self.storage
                .insert_bundle_and_fragments(block_heights, fragments.fragments)
                .await?,
        ))
    }

    async fn submit_fragment(&self, fragment: BundleFragment) -> Result<()> {
        let tx = self.l1_adapter.submit_l2_state(fragment.data).await?;
        self.storage.record_pending_tx(tx, fragment.id).await?;

        Ok(())

        // // TODO: segfault, what about encoding overhead?
        // let (fragment_ids, data) = self.fetch_fragments().await?;
        //
        // // TODO: segfault what about when the fragments don't add up cleanly to max_total_size
        // if data.len() < max_total_size {
        //     let fragment_count = fragment_ids.len();
        //     let data_size = data.len();
        //     let remaining_space = max_total_size.saturating_sub(data_size);
        //
        //     let last_finalization = self
        //         .storage
        //         .last_time_a_fragment_was_finalized()
        //         .await?
        //         .unwrap_or_else(|| {
        //             info!("No fragment has been finalized yet, accumulation timeout will be calculated from the time the committer was started ({})", self.component_created_at);
        //             self.component_created_at
        //         });
        //
        //     let now = self.clock.now();
        //     let time_delta = now - last_finalization;
        //
        //     let duration = time_delta
        //         .to_std()
        //         .unwrap_or_else(|_| {
        //             warn!("possible time skew, last fragment finalization happened at {last_finalization}, with the current clock time at: {now} making for a difference of: {time_delta}");
        //             // we act as if the finalization happened now
        //             Duration::ZERO
        //         });
        //
        //     if duration < self.accumulation_timeout {
        //         info!("Found {fragment_count} fragment(s) with total size of {data_size}B. Waiting for additional fragments to use up more of the remaining {remaining_space}B.");
        //         return Ok(());
        //     } else {
        //         info!("Found {fragment_count} fragment(s) with total size of {data_size}B. Accumulation timeout has expired, proceeding to submit.")
        //     }
        // }
        //
        // if fragment_ids.is_empty() {
        //     return Ok(());
        // }
        //
        // let tx_hash = self.l1_adapter.submit_l2_state(data).await?;
        // self.storage
        //     .record_pending_tx(tx_hash, fragment_ids)
        //     .await?;
        //
        // info!("submitted blob tx {}", hex::encode(tx_hash));
        //
        // Ok(())
    }

    async fn is_tx_pending(&self) -> Result<bool> {
        self.storage.has_pending_txs().await.map_err(|e| e.into())
    }
}

#[async_trait]
impl<L1, Db, C, BF> Runner for StateCommitter<L1, Db, C, BF>
where
    L1: ports::l1::Api + Send + Sync,
    Db: Storage + Clone,
    C: Send + Sync + Clock,
    BF: BundlerFactory + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        if self.is_tx_pending().await? {
            return Ok(());
        };

        let fragment = if let Some(fragment) = self.storage.oldest_nonfinalized_fragment().await? {
            fragment
        } else if let Some(fragments) = self.bundle_then_fragment().await? {
            fragments.take_first()
        } else {
            return Ok(());
        };

        self.submit_fragment(fragment).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[allow(dead_code)]
    fn setup_logger() {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_level(true)
            .with_line_number(true)
            .json()
            .init();
    }

    use std::sync::Arc;

    use clock::TestClock;
    use fuel_crypto::SecretKey;
    use itertools::Itertools;
    use mockall::{predicate::eq, Sequence};
    use ports::{l1::SubmittableFragments, non_empty_vec, types::NonEmptyVec};
    use tokio::sync::Mutex;

    use crate::test_utils::{self, mocks::l1::TxStatus, Blocks};

    use super::*;

    // TODO: segfault add .once() to all tests since mocks dont fail by default if their
    // expectations were not exercised, only if they were exercised incorrectly
    fn random_data(size: usize) -> NonEmptyVec<u8> {
        if size == 0 {
            panic!("random data size must be greater than 0");
        }

        // TODO: segfault use better random data generation
        let data: Vec<u8> = (0..size).map(|_| rand::random::<u8>()).collect();

        data.try_into().expect("is not empty due to check")
    }

    #[tokio::test]
    async fn sends_fragments_in_order() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;

        let fragment_tx_ids = [[0; 32], [1; 32]];

        let mut sut = {
            let fragment_0 = random_data(100);
            let fragment_1 = random_data(100);

            let mut l1_mock = ports::l1::MockApi::new();
            test_utils::mocks::l1::will_split_bundle_into_fragments(
                &mut l1_mock,
                SubmittableFragments {
                    fragments: non_empty_vec![fragment_0.clone(), fragment_1.clone()],
                    gas_estimation: 1,
                },
            );
            let bundler_factory = GasOptimizingBundlerFactory::new(
                Arc::new(l1_mock),
                setup.db(),
                (1..2).try_into().unwrap(),
            );

            let mut l1_mock = ports::l1::MockApi::new();
            let mut sequence = Sequence::new();
            l1_mock
                .expect_submit_l2_state()
                .with(eq(fragment_0))
                .once()
                .return_once(move |_| Ok(fragment_tx_ids[0]))
                .in_sequence(&mut sequence);

            l1_mock
                .expect_submit_l2_state()
                .with(eq(fragment_1))
                .once()
                .return_once(move |_| Ok(fragment_tx_ids[1]))
                .in_sequence(&mut sequence);

            StateCommitter::new(
                l1_mock,
                setup.db(),
                TestClock::default(),
                bundler_factory,
                BundleGenerationConfig {
                    stop_optimization_attempts_after: Duration::from_secs(1),
                },
            )
        };

        setup.import_blocks(Blocks::WithHeights(0..1)).await;
        // sends the first fragment
        sut.run().await.unwrap();
        setup
            .report_txs_finished([(fragment_tx_ids[0], TxStatus::Success)])
            .await;

        // when
        sut.run().await.unwrap();

        // then
        // mocks validate that the second fragment has been sent after the first one

        Ok(())
    }

    #[tokio::test]
    async fn repeats_failed_fragments() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;

        setup.import_blocks(Blocks::WithHeights(0..1)).await;

        let original_tx = [0; 32];

        let mut sut = {
            let fragment_0 = random_data(100);
            let fragment_1 = random_data(100);

            let mut l1_mock = ports::l1::MockApi::new();
            test_utils::mocks::l1::will_split_bundle_into_fragments(
                &mut l1_mock,
                SubmittableFragments {
                    fragments: non_empty_vec![fragment_0.clone(), fragment_1],
                    gas_estimation: 1,
                },
            );
            let bundler_factory = GasOptimizingBundlerFactory::new(
                Arc::new(l1_mock),
                setup.db(),
                (1..2).try_into().unwrap(),
            );

            let mut l1_mock = ports::l1::MockApi::new();
            let retry_tx = [1; 32];
            for tx in [original_tx, retry_tx] {
                l1_mock
                    .expect_submit_l2_state()
                    .with(eq(fragment_0.clone()))
                    .once()
                    .return_once(move |_| Ok(tx));
            }

            StateCommitter::new(
                l1_mock,
                setup.db(),
                TestClock::default(),
                bundler_factory,
                BundleGenerationConfig {
                    stop_optimization_attempts_after: Duration::from_secs(1),
                },
            )
        };

        // Bundles, sends the first fragment
        sut.run().await.unwrap();

        // but the fragment tx fails
        setup
            .report_txs_finished([(original_tx, TxStatus::Failure)])
            .await;

        // when
        // we try again
        sut.run().await.unwrap();

        // then
        // mocks validate that the first fragment has been sent twice

        Ok(())
    }

    #[tokio::test]
    async fn does_nothing_if_not_enough_blocks() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;
        setup.import_blocks(Blocks::WithHeights(0..1)).await;

        let mut sut = {
            let l1_mock = ports::l1::MockApi::new();
            StateCommitter::new(
                l1_mock,
                setup.db(),
                TestClock::default(),
                GasOptimizingBundlerFactory::new(
                    Arc::new(ports::l1::MockApi::new()),
                    setup.db(),
                    (2..3).try_into().unwrap(),
                ),
                BundleGenerationConfig {
                    stop_optimization_attempts_after: Duration::from_secs(1),
                },
            )
        };

        // when
        sut.run().await.unwrap();

        // then
        // mocks will validate nothing happened

        Ok(())
    }

    #[tokio::test]
    async fn does_nothing_if_there_are_pending_transactions() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;

        setup.import_blocks(Blocks::WithHeights(0..2)).await;

        let mut sut = {
            let mut l1_mock = ports::l1::MockApi::new();
            test_utils::mocks::l1::will_split_bundle_into_fragments(
                &mut l1_mock,
                SubmittableFragments {
                    fragments: non_empty_vec![random_data(100)],
                    gas_estimation: 1,
                },
            );
            let bundler_factory = GasOptimizingBundlerFactory::new(
                Arc::new(l1_mock),
                setup.db(),
                (1..2).try_into().unwrap(),
            );

            let mut l1_mock = ports::l1::MockApi::new();
            l1_mock
                .expect_submit_l2_state()
                .once()
                .return_once(|_| Ok([1; 32]));

            StateCommitter::new(
                l1_mock,
                setup.db(),
                TestClock::default(),
                bundler_factory,
                BundleGenerationConfig {
                    stop_optimization_attempts_after: Duration::from_secs(1),
                },
            )
        };

        // bundles and sends the first block
        sut.run().await.unwrap();

        // when
        sut.run().await.unwrap();

        // then
        // mocks didn't catch any additional calls
        Ok(())
    }

    #[tokio::test]
    async fn bundles_minimum_acceptable_if_no_more_blocks_available() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;

        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = (0..2)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: blocks.clone(),
                secret_key,
            })
            .await;

        let mut sut = {
            let mut l1_mock = ports::l1::MockApi::new();
            let fragment = random_data(100);
            let encoded_blocks: Vec<ports::storage::FuelBlock> = blocks
                .into_iter()
                .map(TryFrom::try_from)
                .try_collect()
                .unwrap();

            let two_block_bundle = encoded_blocks
                .into_iter()
                .flat_map(|b| b.data.into_inner())
                .collect::<Vec<_>>();

            {
                let fragment = fragment.clone();
                l1_mock
                    .expect_split_into_submittable_fragments()
                    .withf(move |data| data.inner() == &two_block_bundle)
                    .once()
                    .return_once(|_| {
                        Ok(SubmittableFragments {
                            fragments: non_empty_vec![fragment],
                            gas_estimation: 1,
                        })
                    });
            }
            let factory = GasOptimizingBundlerFactory::new(
                Arc::new(l1_mock),
                setup.db(),
                (2..3).try_into().unwrap(),
            );

            let mut l1_mock = ports::l1::MockApi::new();
            l1_mock
                .expect_submit_l2_state()
                .with(eq(fragment.clone()))
                .once()
                .return_once(|_| Ok([1; 32]));

            StateCommitter::new(
                l1_mock,
                setup.db(),
                TestClock::default(),
                factory,
                BundleGenerationConfig {
                    stop_optimization_attempts_after: Duration::from_secs(1),
                },
            )
        };

        // when
        sut.run().await.unwrap();

        // then
        // mocks validate that the bundle was comprised of two blocks

        Ok(())
    }

    #[tokio::test]
    async fn doesnt_bundle_more_than_maximum_blocks() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = (0..3)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: blocks.clone(),
                secret_key,
            })
            .await;

        let mut sut = {
            let mut l1_mock = ports::l1::MockApi::new();
            let encoded_blocks: Vec<ports::storage::FuelBlock> = blocks
                .into_iter()
                .map(TryFrom::try_from)
                .try_collect()
                .unwrap();

            let two_block_bundle = encoded_blocks
                .into_iter()
                .take(2)
                .flat_map(|b| b.data.into_inner())
                .collect::<Vec<_>>();

            let fragment = random_data(100);
            {
                let fragment = fragment.clone();
                l1_mock
                    .expect_split_into_submittable_fragments()
                    .withf(move |data| data.inner() == &two_block_bundle)
                    .once()
                    .return_once(|_| {
                        Ok(SubmittableFragments {
                            fragments: non_empty_vec![fragment],
                            gas_estimation: 1,
                        })
                    });
            }
            let factory = GasOptimizingBundlerFactory::new(
                Arc::new(l1_mock),
                setup.db(),
                (2..3).try_into().unwrap(),
            );
            let mut l1_mock = ports::l1::MockApi::new();

            l1_mock
                .expect_submit_l2_state()
                .with(eq(fragment.clone()))
                .once()
                .return_once(|_| Ok([1; 32]));

            StateCommitter::new(
                l1_mock,
                setup.db(),
                TestClock::default(),
                factory,
                BundleGenerationConfig {
                    stop_optimization_attempts_after: Duration::from_secs(1),
                },
            )
        };

        // when
        sut.run().await.unwrap();

        // then
        // mocks validate that the bundle was comprised of two blocks even though three were available

        Ok(())
    }

    #[tokio::test]
    async fn doesnt_bundle_already_bundled_blocks() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let blocks = (0..=1)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: blocks.clone(),
                secret_key,
            })
            .await;

        let bundle_1_tx = [0; 32];
        let bundle_2_tx = [1; 32];
        let mut sut = {
            let mut sequence = Sequence::new();
            let mut l1_mock = ports::l1::MockApi::new();
            let bundle_1_fragment = random_data(100);
            {
                let bundle_1 = ports::storage::FuelBlock::try_from(blocks[0].clone())
                    .unwrap()
                    .data;
                let fragments = non_empty_vec![bundle_1_fragment.clone()];
                l1_mock
                    .expect_split_into_submittable_fragments()
                    .withf(move |data| data.inner() == bundle_1.inner())
                    .once()
                    .return_once(|_| {
                        Ok(SubmittableFragments {
                            fragments,
                            gas_estimation: 1,
                        })
                    })
                    .in_sequence(&mut sequence);
            }

            let bundle_2_fragment = random_data(100);
            {
                let bundle_2 = ports::storage::FuelBlock::try_from(blocks[1].clone())
                    .unwrap()
                    .data;
                let fragments = non_empty_vec!(bundle_2_fragment.clone());
                l1_mock
                    .expect_split_into_submittable_fragments()
                    .withf(move |data| data.inner() == bundle_2.inner())
                    .once()
                    .return_once(move |_| {
                        Ok(SubmittableFragments {
                            fragments,
                            gas_estimation: 1,
                        })
                    })
                    .in_sequence(&mut sequence);
            }

            let bundler_factory = GasOptimizingBundlerFactory::new(
                Arc::new(l1_mock),
                setup.db(),
                (1..2).try_into().unwrap(),
            );

            let mut sequence = Sequence::new();
            let mut l1_mock = ports::l1::MockApi::new();
            l1_mock
                .expect_submit_l2_state()
                .with(eq(bundle_1_fragment.clone()))
                .once()
                .return_once(move |_| Ok(bundle_1_tx))
                .in_sequence(&mut sequence);

            l1_mock
                .expect_submit_l2_state()
                .with(eq(bundle_2_fragment.clone()))
                .once()
                .return_once(move |_| Ok(bundle_2_tx))
                .in_sequence(&mut sequence);

            StateCommitter::new(
                l1_mock,
                setup.db(),
                TestClock::default(),
                bundler_factory,
                BundleGenerationConfig {
                    stop_optimization_attempts_after: Duration::from_secs(1),
                },
            )
        };

        // bundles and sends the first block
        sut.run().await.unwrap();

        setup
            .report_txs_finished([(bundle_1_tx, TxStatus::Success)])
            .await;

        // when
        sut.run().await.unwrap();

        // then
        // mocks validate that the second block was bundled and sent

        Ok(())
    }

    #[tokio::test]
    async fn can_be_disabled_by_giving_an_empty_acceptable_block_range() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;

        let mut sut = StateCommitter::new(
            ports::l1::MockApi::new(),
            setup.db(),
            TestClock::default(),
            GasOptimizingBundlerFactory::new(
                Arc::new(ports::l1::MockApi::new()),
                setup.db(),
                (0..1).try_into().unwrap(),
            ),
            BundleGenerationConfig {
                stop_optimization_attempts_after: Duration::from_secs(1),
            },
        );

        // when
        sut.run().await.unwrap();

        // then
        // no calls to mocks were made

        Ok(())
    }

    #[tokio::test]
    async fn optimizes_for_gas_usage() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;
        let secret_key = SecretKey::random(&mut rand::thread_rng());

        let blocks = (0..=3)
            .map(|height| test_utils::mocks::fuel::generate_block(height, &secret_key))
            .collect_vec();

        setup
            .import_blocks(Blocks::Blocks {
                blocks: blocks.clone(),
                secret_key,
            })
            .await;

        let mut sut = {
            let first_bundle = (0..=1)
                .flat_map(|i| {
                    ports::storage::FuelBlock::try_from(blocks[i].clone())
                        .unwrap()
                        .data
                        .into_inner()
                })
                .collect::<Vec<_>>();
            let second_bundle = (0..=2)
                .flat_map(|i| {
                    ports::storage::FuelBlock::try_from(blocks[i].clone())
                        .unwrap()
                        .data
                        .into_inner()
                })
                .collect::<Vec<_>>();
            let third_bundle = (0..=3)
                .flat_map(|i| {
                    ports::storage::FuelBlock::try_from(blocks[i].clone())
                        .unwrap()
                        .data
                        .into_inner()
                })
                .collect::<Vec<_>>();

            let mut sequence = Sequence::new();

            let correct_fragment = random_data(100);

            let mut l1_mock = ports::l1::MockApi::new();
            l1_mock
                .expect_split_into_submittable_fragments()
                .withf(move |data| data.inner() == &first_bundle)
                .once()
                .return_once(|_| {
                    Ok(SubmittableFragments {
                        fragments: non_empty_vec![random_data(100)],
                        gas_estimation: 2,
                    })
                })
                .in_sequence(&mut sequence);

            {
                let fragments = non_empty_vec![correct_fragment.clone()];
                l1_mock
                    .expect_split_into_submittable_fragments()
                    .withf(move |data| data.inner() == &second_bundle)
                    .once()
                    .return_once(|_| {
                        Ok(SubmittableFragments {
                            fragments,
                            gas_estimation: 1,
                        })
                    })
                    .in_sequence(&mut sequence);
            }

            l1_mock
                .expect_split_into_submittable_fragments()
                .withf(move |data| data.inner() == &third_bundle)
                .once()
                .return_once(|_| {
                    Ok(SubmittableFragments {
                        fragments: non_empty_vec![random_data(100)],
                        gas_estimation: 3,
                    })
                })
                .in_sequence(&mut sequence);

            let bundler_factory = GasOptimizingBundlerFactory::new(
                Arc::new(l1_mock),
                setup.db(),
                (2..5).try_into().unwrap(),
            );

            let mut l1_mock = ports::l1::MockApi::new();

            l1_mock
                .expect_submit_l2_state()
                .with(eq(correct_fragment.clone()))
                .once()
                .return_once(move |_| Ok([0; 32]))
                .in_sequence(&mut sequence);

            StateCommitter::new(
                l1_mock,
                setup.db(),
                TestClock::default(),
                bundler_factory,
                BundleGenerationConfig {
                    stop_optimization_attempts_after: Duration::from_secs(1),
                },
            )
        };

        // when
        sut.run().await.unwrap();

        // then
        // mocks validate that the bundle including blocks 0,1 and 2 was chosen having the best gas
        // per byte

        Ok(())
    }

    #[tokio::test]
    async fn stops_asking_for_optimizations_if_time_exhausted() -> Result<()> {
        //given
        let setup = test_utils::Setup::init().await;

        struct TestBundler {
            rx: tokio::sync::mpsc::Receiver<Bundle>,
        }

        #[async_trait::async_trait]
        impl Bundler for TestBundler {
            async fn propose_bundle(&mut self) -> Result<Option<Bundle>> {
                Ok(__self.rx.recv().await)
            }
        }
        struct TestBundlerFactory {
            bundler: Mutex<Option<TestBundler>>,
        }

        #[async_trait::async_trait]
        impl BundlerFactory for TestBundlerFactory {
            type Bundler = TestBundler;

            async fn build(&self) -> Result<Self::Bundler> {
                Ok(self.bundler.lock().await.take().unwrap())
            }
        }

        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let test_bundler = TestBundler { rx };
        let factory = TestBundlerFactory {
            bundler: Mutex::new(Some(test_bundler)),
        };

        let test_clock = TestClock::default();
        let second_optimization_run_fragment = non_empty_vec!(1);
        let mut sut = {
            let mut l1_mock = ports::l1::MockApi::new();

            l1_mock
                .expect_submit_l2_state()
                .with(eq(second_optimization_run_fragment.clone()))
                .return_once(move |_| Ok([0; 32]));

            StateCommitter::new(
                l1_mock,
                setup.db(),
                test_clock.clone(),
                factory,
                BundleGenerationConfig {
                    stop_optimization_attempts_after: Duration::from_secs(1),
                },
            )
        };

        let sut_task = tokio::task::spawn(async move {
            sut.run().await.unwrap();
        });

        tx.send(Bundle {
            fragments: SubmittableFragments {
                fragments: non_empty_vec!(non_empty_vec!(0)),
                gas_estimation: 1,
            },
            block_heights: (0..1).try_into().unwrap(),
            optimal: false,
        })
        .await
        .unwrap();

        test_clock.adv_time(Duration::from_secs(1)).await;

        // when
        tx.send(Bundle {
            fragments: SubmittableFragments {
                fragments: non_empty_vec!(second_optimization_run_fragment.clone()),
                gas_estimation: 1,
            },
            block_heights: (0..1).try_into().unwrap(),
            optimal: false,
        })
        .await
        .unwrap();
        drop(tx);

        // then
        // the second, albeit unoptimized, bundle gets sent to l1
        tokio::time::timeout(Duration::from_secs(1), sut_task)
            .await
            .unwrap()
            .unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn gas_optimizing_bundler_reports_nonoptimal_bundles_as_well() -> Result<()> {
        // given
        let secret_key = SecretKey::random(&mut rand::thread_rng());
        let blocks = (0..=3)
            .map(|height| test_utils::mocks::fuel::generate_storage_block(height, &secret_key))
            .collect_vec();

        let bundle_of_blocks_0_and_1: NonEmptyVec<u8> = blocks[0..=1]
            .iter()
            .flat_map(|block| block.data.clone().into_inner())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        let mut l1_mock = ports::l1::MockApi::new();
        let fragment_of_unoptimal_block = random_data(100);
        {
            let fragments = non_empty_vec![fragment_of_unoptimal_block.clone()];
            l1_mock
                .expect_split_into_submittable_fragments()
                .with(eq(bundle_of_blocks_0_and_1))
                .once()
                .return_once(|_| {
                    Ok(SubmittableFragments {
                        fragments,
                        gas_estimation: 100,
                    })
                });
        }

        let mut sut = GasOptimizingBundler::new(l1_mock, blocks, (2..4).try_into().unwrap());

        // when
        let bundle = sut.propose_bundle().await.unwrap().unwrap();

        // then
        assert_eq!(
            bundle,
            Bundle {
                fragments: SubmittableFragments {
                    fragments: non_empty_vec!(fragment_of_unoptimal_block),
                    gas_estimation: 100
                },
                block_heights: (0..2).try_into().unwrap(),
                optimal: false
            }
        );

        Ok(())
    }
}
