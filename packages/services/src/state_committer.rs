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

pub struct NonCompressingGasOptimizingBundler<L1, Storage> {
    l1_adapter: L1,
    storage: Storage,
    acceptable_amount_of_blocks: ValidatedRange<usize>,
}

impl<L1, Storage> NonCompressingGasOptimizingBundler<L1, Storage> {
    fn new(
        l1_adapter: L1,
        storage: Storage,
        acceptable_amount_of_blocks: ValidatedRange<usize>,
    ) -> Self {
        Self {
            l1_adapter,
            storage,
            acceptable_amount_of_blocks,
        }
    }
}

struct Bundle {
    pub fragments: SubmittableFragments,
    pub block_heights: ValidatedRange<u32>,
    pub optimal: bool,
}

#[async_trait::async_trait]
trait Bundler {
    async fn propose_bundle(&mut self) -> Result<Option<Bundle>>;
}

trait BundlerFactory<L1, Storage> {
    type Bundler: Bundler;
    fn build(&self, db: Storage, l1: L1) -> Self::Bundler;
}

struct NonCompressingGasOptimizingBundlerFactory {
    acceptable_amount_of_blocks: ValidatedRange<usize>,
}

impl NonCompressingGasOptimizingBundlerFactory {
    pub fn new(acceptable_amount_of_blocks: ValidatedRange<usize>) -> Self {
        Self {
            acceptable_amount_of_blocks,
        }
    }
}

impl<L1, Storage> BundlerFactory<L1, Storage> for NonCompressingGasOptimizingBundlerFactory
where
    NonCompressingGasOptimizingBundler<L1, Storage>: Bundler,
{
    type Bundler = NonCompressingGasOptimizingBundler<L1, Storage>;
    fn build(&self, storage: Storage, l1: L1) -> Self::Bundler {
        NonCompressingGasOptimizingBundler::new(
            l1,
            storage,
            self.acceptable_amount_of_blocks.clone(),
        )
    }
}

#[async_trait::async_trait]
impl<L1, Storage> Bundler for NonCompressingGasOptimizingBundler<L1, Storage>
where
    L1: ports::l1::Api + Send + Sync,
    Storage: ports::storage::Storage,
{
    async fn propose_bundle(&mut self) -> Result<Option<Bundle>> {
        let max_blocks = self
            .acceptable_amount_of_blocks
            .inner()
            .clone()
            .max()
            .unwrap_or(0);

        let blocks: Vec<_> = self
            .storage
            .stream_unbundled_blocks()
            .take(max_blocks)
            .try_collect()
            .await?;

        if blocks.is_empty() {
            eprintln!("no blocks found");
            return Ok(None);
        }

        if !self.acceptable_amount_of_blocks.contains(blocks.len()) {
            eprintln!("not enough blocks found");
            return Ok(None);
        }

        let mut gas_usage_tracking = HashMap::new();

        for amount_of_blocks in self.acceptable_amount_of_blocks.inner().clone() {
            eprintln!("trying amount of blocks: {}", amount_of_blocks);
            let merged_data = blocks[..amount_of_blocks]
                .iter()
                .flat_map(|b| b.data.clone().into_inner())
                .collect::<Vec<_>>();

            let submittable_chunks = self.l1_adapter.split_into_submittable_fragments(
                &merged_data.try_into().expect("cannot be empty"),
            )?;
            eprintln!(
                "submittable chunks gas: {:?}",
                submittable_chunks.gas_estimation
            );

            gas_usage_tracking.insert(amount_of_blocks, submittable_chunks);
        }

        let (amount_of_blocks, fragments) = gas_usage_tracking
            .into_iter()
            .min_by_key(|(_, chunks)| chunks.gas_estimation)
            .unwrap();
        eprintln!("chosen amount of blocks: {}", amount_of_blocks);
        eprintln!("chosen gas usage: {:?}", fragments.gas_estimation);

        let (min_height, max_height) = blocks.as_slice()[..amount_of_blocks]
            .iter()
            .map(|b| b.height)
            .minmax()
            .into_option()
            .unwrap();
        eprintln!("min height: {}, max height: {}", min_height, max_height);

        let block_heights = (min_height..max_height + 1).try_into().unwrap();

        Ok(Some(Bundle {
            fragments,
            block_heights,
            optimal: true,
        }))
    }
}

pub struct StateCommitter<L1, Storage, Clock, BundlerFactory> {
    l1_adapter: L1,
    storage: Storage,
    clock: Clock,
    component_created_at: DateTime<Utc>,
    bundler_factory: BundlerFactory,
}

impl<L1, Storage, C, BF> StateCommitter<L1, Storage, C, BF>
where
    C: Clock,
{
    pub fn new(l1_adapter: L1, storage: Storage, clock: C, bundler_factory: BF) -> Self {
        let now = clock.now();

        Self {
            l1_adapter,
            storage,
            clock,
            component_created_at: now,
            bundler_factory,
        }
    }
}

impl<L1, Db, C, BF> StateCommitter<L1, Db, C, BF>
where
    L1: ports::l1::Api,
    Db: Storage,
    C: Clock,
    BF: BundlerFactory<L1, Db>,
{
    async fn submit_state(&self, fragment: BundleFragment) -> Result<()> {
        eprintln!("submitting state: {:?}", fragment);
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
    L1: ports::l1::Api + Send + Sync + Clone,
    Db: Storage + Clone,
    C: Send + Sync + Clock,
    BF: BundlerFactory<L1, Db, Bundler: Send> + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        eprintln!("running committer");
        if self.is_tx_pending().await? {
            eprintln!("tx pending, returning");
            return Ok(());
        };

        let fragment = if let Some(fragment) = self.storage.oldest_nonfinalized_fragment().await? {
            eprintln!("found fragment: {:?}", fragment);
            fragment
        } else {
            eprintln!("no fragment found");
            let mut bundler = self
                .bundler_factory
                .build(self.storage.clone(), self.l1_adapter.clone());

            if let Some(Bundle {
                fragments,
                block_heights,
                optimal,
            }) = bundler.propose_bundle().await?
            {
                // TODO: segfault, change unwraps to ? wherever possible
                self.storage
                    .insert_bundle_and_fragments(block_heights, fragments.fragments)
                    .await?
                    .into_inner()
                    .into_iter()
                    .next()
                    .expect("must have at least one element due to the usage of NonEmptyVec")
            } else {
                eprintln!("no bundle found");
                return Ok(());
            }
        };

        self.submit_state(fragment).await?;

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

    use crate::{
        test_utils::{self, mocks::l1::TxStatus, Blocks},
        StateListener,
    };

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
            let mut l1_mock = ports::l1::MockApi::new();

            let fragment_0 = random_data(100);
            let fragment_1 = random_data(100);

            {
                let fragments = non_empty_vec![fragment_0.clone(), fragment_1.clone()];
                l1_mock
                    .expect_split_into_submittable_fragments()
                    .once()
                    .return_once(move |_| {
                        Ok(SubmittableFragments {
                            fragments,
                            gas_estimation: 1,
                        })
                    });
            }

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

            let bundler_factory =
                NonCompressingGasOptimizingBundlerFactory::new((1..2).try_into().unwrap());

            StateCommitter::new(
                Arc::new(l1_mock),
                setup.db(),
                TestClock::default(),
                bundler_factory,
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
            let mut l1_mock = ports::l1::MockApi::new();
            let fragment_0 = random_data(100);
            let fragment_1 = random_data(100);
            {
                let fragments = non_empty_vec![fragment_0.clone(), fragment_1];

                l1_mock
                    .expect_split_into_submittable_fragments()
                    .once()
                    .return_once(move |_| {
                        Ok(SubmittableFragments {
                            fragments,
                            gas_estimation: 1,
                        })
                    });
            }

            let retry_tx = [1; 32];
            for tx in [original_tx, retry_tx] {
                l1_mock
                    .expect_submit_l2_state()
                    .with(eq(fragment_0.clone()))
                    .once()
                    .return_once(move |_| Ok(tx));
            }

            StateCommitter::new(
                Arc::new(l1_mock),
                setup.db(),
                TestClock::default(),
                NonCompressingGasOptimizingBundlerFactory::new((1..2).try_into().unwrap()),
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
                Arc::new(l1_mock),
                setup.db(),
                TestClock::default(),
                NonCompressingGasOptimizingBundlerFactory::new((2..3).try_into().unwrap()),
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
            l1_mock
                .expect_split_into_submittable_fragments()
                .once()
                .return_once(|_| {
                    Ok(SubmittableFragments {
                        fragments: non_empty_vec!(random_data(100)),
                        gas_estimation: 1,
                    })
                });

            l1_mock
                .expect_submit_l2_state()
                .once()
                .return_once(|_| Ok([1; 32]));
            StateCommitter::new(
                Arc::new(l1_mock),
                setup.db(),
                TestClock::default(),
                NonCompressingGasOptimizingBundlerFactory::new((1..2).try_into().unwrap()),
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

            l1_mock
                .expect_submit_l2_state()
                .with(eq(fragment.clone()))
                .once()
                .return_once(|_| Ok([1; 32]));

            StateCommitter::new(
                Arc::new(l1_mock),
                setup.db(),
                TestClock::default(),
                NonCompressingGasOptimizingBundlerFactory::new((2..3).try_into().unwrap()),
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

            l1_mock
                .expect_submit_l2_state()
                .with(eq(fragment.clone()))
                .once()
                .return_once(|_| Ok([1; 32]));

            StateCommitter::new(
                Arc::new(l1_mock),
                setup.db(),
                TestClock::default(),
                NonCompressingGasOptimizingBundlerFactory::new((2..3).try_into().unwrap()),
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
            let mut l1_mock = ports::l1::MockApi::new();

            let bundle_1 = ports::storage::FuelBlock::try_from(blocks[0].clone())
                .unwrap()
                .data;
            let mut sequence = Sequence::new();

            let fragment = random_data(100);
            {
                let fragments = non_empty_vec![fragment.clone()];
                l1_mock
                    .expect_split_into_submittable_fragments()
                    .withf(move |data| {
                        println!("data #1: {:?}", data);
                        data.inner() == bundle_1.inner()
                    })
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
                .expect_submit_l2_state()
                .with(eq(fragment.clone()))
                .once()
                .return_once(move |_| Ok(bundle_1_tx))
                .in_sequence(&mut sequence);

            let bundle_2 = ports::storage::FuelBlock::try_from(blocks[1].clone())
                .unwrap()
                .data;

            let fragment = random_data(100);
            {
                let fragments = non_empty_vec!(fragment.clone());
                l1_mock
                    .expect_split_into_submittable_fragments()
                    .withf(move |data| {
                        println!("data #2: {:?}", data);
                        data.inner() == bundle_2.inner()
                    })
                    .once()
                    .return_once(move |_| {
                        Ok(SubmittableFragments {
                            fragments,
                            gas_estimation: 1,
                        })
                    })
                    .in_sequence(&mut sequence);
            }
            l1_mock
                .expect_submit_l2_state()
                .with(eq(fragment.clone()))
                .once()
                .return_once(move |_| Ok(bundle_2_tx))
                .in_sequence(&mut sequence);

            StateCommitter::new(
                Arc::new(l1_mock),
                setup.db(),
                TestClock::default(),
                NonCompressingGasOptimizingBundlerFactory::new((1..2).try_into().unwrap()),
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
            Arc::new(ports::l1::MockApi::new()),
            setup.db(),
            TestClock::default(),
            NonCompressingGasOptimizingBundlerFactory::new((0..1).try_into().unwrap()),
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
            let mut l1_mock = ports::l1::MockApi::new();

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

            l1_mock
                .expect_submit_l2_state()
                .with(eq(correct_fragment.clone()))
                .once()
                .return_once(move |_| Ok([0; 32]))
                .in_sequence(&mut sequence);

            StateCommitter::new(
                Arc::new(l1_mock),
                setup.db(),
                TestClock::default(),
                NonCompressingGasOptimizingBundlerFactory::new((2..5).try_into().unwrap()),
            )
        };

        // when
        sut.run().await.unwrap();

        // then
        // mocks validate that the bundle including blocks 0,1 and 2 was chosen having the best gas
        // per byte

        Ok(())
    }
}
