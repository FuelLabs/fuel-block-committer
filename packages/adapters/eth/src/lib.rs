use std::{
    num::{NonZeroU32, NonZeroUsize},
    ops::RangeInclusive,
    time::Duration,
};

use alloy::{
    consensus::BlobTransactionSidecar,
    eips::eip4844::{BYTES_PER_BLOB, DATA_GAS_PER_BLOB},
    primitives::U256,
    rpc::types::FeeHistory,
};
use delegate::delegate;
use futures::{stream, StreamExt, TryStreamExt};
use itertools::{izip, Itertools};
use services::{
    fee_analytics::port::{l1::SequentialBlockFees, BlockFees, Fees},
    types::{
        BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Height, L1Tx, NonEmpty, NonNegative,
        TransactionResponse,
    },
    Result,
};

mod aws;
mod error;
mod metrics;
mod websocket;

pub use alloy::primitives::Address;
pub use aws::*;
use fuel_block_committer_encoding::blob::{self, generate_sidecar};
use static_assertions::const_assert;
pub use websocket::{L1Key, L1Keys, Signer, Signers, TxConfig, WebsocketClient};

#[derive(Debug, Copy, Clone)]
pub struct BlobEncoder;

pub async fn make_pub_eth_client() -> WebsocketClient {
    let signers = Signers::for_keys(crate::L1Keys {
        main: crate::L1Key::Private(
            "98d88144512cc5747fed20bdc81fb820c4785f7411bd65a88526f3b084dc931e".to_string(),
        ),
        blob: None,
    })
    .await
    .unwrap();

    crate::WebsocketClient::connect(
        "wss://ethereum-rpc.publicnode.com".parse().unwrap(),
        Default::default(),
        signers,
        10,
        crate::TxConfig {
            tx_max_fee: u128::MAX,
            send_tx_request_timeout: Duration::MAX,
        },
    )
    .await
    .unwrap()
}
impl BlobEncoder {
    #[cfg(feature = "test-helpers")]
    pub const FRAGMENT_SIZE: usize = BYTES_PER_BLOB;

    pub(crate) fn sidecar_from_fragments(
        fragments: impl IntoIterator<Item = Fragment>,
    ) -> crate::error::Result<BlobTransactionSidecar> {
        let mut sidecar = BlobTransactionSidecar::default();

        for fragment in fragments {
            let data = Vec::from(fragment.data);

            sidecar.blobs.push(Default::default());
            let current_blob = sidecar.blobs.last_mut().expect("just added it");

            sidecar.commitments.push(Default::default());
            let current_commitment = sidecar.commitments.last_mut().expect("just added it");

            sidecar.proofs.push(Default::default());
            let current_proof = sidecar.proofs.last_mut().expect("just added it");

            let read_location = data.as_slice();

            current_blob.copy_from_slice(&read_location[..BYTES_PER_BLOB]);
            let read_location = &read_location[BYTES_PER_BLOB..];

            current_commitment.copy_from_slice(&read_location[..48]);
            let read_location = &read_location[48..];

            current_proof.copy_from_slice(&read_location[..48]);
        }

        Ok(sidecar)
    }
}

impl services::block_bundler::port::l1::FragmentEncoder for BlobEncoder {
    fn encode(&self, data: NonEmpty<u8>, id: NonNegative<i32>) -> Result<NonEmpty<Fragment>> {
        let data = Vec::from(data);
        let encoder = blob::Encoder::default();
        let decoder = blob::Decoder::default();

        let blobs = encoder.encode(&data, id.as_u32()).map_err(|e| {
            crate::error::Error::Other(format!("failed to encode data as blobs: {e}"))
        })?;

        let bits_usage: Vec<_> = blobs
            .iter()
            .map(|blob| {
                let blob::Header::V1(header) = decoder.read_header(blob).map_err(|e| {
                    crate::error::Error::Other(format!("failed to read blob header: {e}"))
                })?;
                Result::Ok(header.num_bits)
            })
            .try_collect()?;

        let sidecar = generate_sidecar(blobs)
            .map_err(|e| crate::error::Error::Other(format!("failed to generate sidecar: {e}")))?;

        let fragments = izip!(
            &sidecar.blobs,
            &sidecar.commitments,
            &sidecar.proofs,
            bits_usage
        )
        .map(|(blob, commitment, proof, used_bits)| {
            let mut data_commitment_and_proof = vec![0; blob.len() + 48 * 2];
            let write_location = &mut data_commitment_and_proof[..];

            write_location[..blob.len()].copy_from_slice(blob.as_slice());
            let write_location = &mut write_location[blob.len()..];

            write_location[..48].copy_from_slice(&(**commitment));
            let write_location = &mut write_location[48..];

            write_location[..48].copy_from_slice(&(**proof));

            let bits_per_blob = BYTES_PER_BLOB as u32 * 8;

            Fragment {
                data: NonEmpty::from_vec(data_commitment_and_proof).expect("known to be non-empty"),
                unused_bytes: bits_per_blob.saturating_sub(used_bits).saturating_div(8),
                total_bytes: bits_per_blob
                    .saturating_div(8)
                    .try_into()
                    .expect("known to be non-zero"),
            }
        })
        .collect();

        Ok(NonEmpty::from_vec(fragments).expect("known to be non-empty"))
    }

    fn gas_usage(&self, num_bytes: NonZeroUsize) -> u64 {
        blob::Encoder::default().blobs_needed_to_encode(num_bytes.get()) as u64 * DATA_GAS_PER_BLOB
    }
}

impl services::block_committer::port::l1::Contract for WebsocketClient {
    delegate! {
        to self {
            async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;
            fn commit_interval(&self) -> NonZeroU32;
        }
    }
}

impl services::state_listener::port::l1::Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn get_transaction_response(&self, tx_hash: [u8; 32],) -> Result<Option<TransactionResponse>>;
            async fn is_squeezed_out(&self, tx_hash: [u8; 32],) -> Result<bool>;
        }
    }

    async fn get_block_number(&self) -> Result<L1Height> {
        let block_num = self._get_block_number().await?;
        let height = L1Height::try_from(block_num)?;

        Ok(height)
    }
}

impl services::wallet_balance_tracker::port::l1::Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn balance(&self, address: Address) -> Result<U256>;
        }
    }
}

impl services::block_committer::port::l1::Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn get_transaction_response(&self, tx_hash: [u8; 32],) -> Result<Option<TransactionResponse>>;
        }
    }

    async fn get_block_number(&self) -> Result<L1Height> {
        let block_num = self._get_block_number().await?;
        let height = L1Height::try_from(block_num)?;

        Ok(height)
    }
}

impl services::state_committer::port::l1::Api for WebsocketClient {
    async fn current_height(&self) -> Result<u64> {
        self._get_block_number().await
    }

    delegate! {
        to (*self) {
            async fn submit_state_fragments(
                &self,
                fragments: NonEmpty<Fragment>,
                previous_tx: Option<services::types::L1Tx>,
            ) -> Result<(L1Tx, FragmentsSubmitted)>;
        }
    }
}

impl services::fee_analytics::port::l1::FeesProvider for WebsocketClient {
    async fn fees(&self, height_range: RangeInclusive<u64>) -> Result<SequentialBlockFees> {
        const REWARD_PERCENTILE: f64 =
            alloy::providers::utils::EIP1559_FEE_ESTIMATION_REWARD_PERCENTILE;
        // so that a alloy version bump doesn't surprise us
        const_assert!(REWARD_PERCENTILE == 20.0,);

        // There is a comment in alloy about not doing more than 1024 blocks at a time
        const RPC_LIMIT: u64 = 1024;

        let fees: Vec<FeeHistory> = stream::iter(chunk_range_inclusive(height_range, RPC_LIMIT))
            .then(|range| self.fees(range, std::slice::from_ref(&REWARD_PERCENTILE)))
            .try_collect()
            .await?;

        let mut unpacked_fees = vec![];
        for fee in fees {
            unpacked_fees.extend(unpack_fee_history(fee)?);
        }

        unpacked_fees
            .try_into()
            .map_err(|e| services::Error::Other(format!("{e}")))
    }

    async fn current_block_height(&self) -> Result<u64> {
        self._get_block_number().await
    }
}

fn unpack_fee_history(fees: FeeHistory) -> Result<Vec<BlockFees>> {
    let number_of_blocks = if fees.base_fee_per_gas.is_empty() {
        0
    } else {
        // We subtract 1 because the last element is the expected fee for the next block
        fees.base_fee_per_gas
            .len()
            .checked_sub(1)
            .expect("checked not 0")
    };

    if number_of_blocks == 0 {
        return Ok(vec![]);
    }

    let Some(nested_rewards) = fees.reward.as_ref() else {
        return Err(services::Error::Other(format!(
            "missing rewards field: {fees:?}"
        )));
    };

    if number_of_blocks != nested_rewards.len()
        || number_of_blocks != fees.base_fee_per_blob_gas.len() - 1
    {
        return Err(services::Error::Other(format!(
            "discrepancy in lengths of fee fields: {fees:?}"
        )));
    }

    let rewards: Vec<_> = nested_rewards
        .iter()
        .map(|perc| {
            perc.last().copied().ok_or_else(|| {
                crate::error::Error::Other(
                    "should have had at least one reward percentile".to_string(),
                )
            })
        })
        .try_collect()?;

    let values = izip!(
        (fees.oldest_block..),
        fees.base_fee_per_gas.into_iter(),
        fees.base_fee_per_blob_gas.into_iter(),
        rewards
    )
    .take(number_of_blocks)
    .map(
        |(height, base_fee_per_gas, base_fee_per_blob_gas, reward)| BlockFees {
            height,
            fees: Fees {
                base_fee_per_gas,
                reward,
                base_fee_per_blob_gas,
            },
        },
    )
    .collect();

    Ok(values)
}

fn chunk_range_inclusive(
    initial_range: RangeInclusive<u64>,
    chunk_size: u64,
) -> Vec<std::ops::RangeInclusive<u64>> {
    let mut ranges = Vec::new();

    if chunk_size == 0 {
        return ranges;
    }

    let start = *initial_range.start();
    let end = *initial_range.end();

    let mut current = start;
    while current <= end {
        // Calculate the end of the current chunk.
        let chunk_end = (current + chunk_size - 1).min(end);

        ranges.push(current..=chunk_end);

        current = chunk_end + 1;
    }

    ranges
}

#[cfg(test)]
mod test {
    use super::chunk_range_inclusive;
    use alloy::{eips::eip4844::DATA_GAS_PER_BLOB, rpc::types::FeeHistory};
    use fuel_block_committer_encoding::blob;
    use services::{
        block_bundler::port::l1::FragmentEncoder,
        fee_analytics::port::{BlockFees, Fees},
    };
    use std::ops::RangeInclusive;

    use crate::{unpack_fee_history, BlobEncoder};

    #[test]
    fn gas_usage_correctly_calculated() {
        // given
        let num_bytes = 400_000;
        let encoder = blob::Encoder::default();
        assert_eq!(encoder.blobs_needed_to_encode(num_bytes), 4);

        // when
        let gas_usage = BlobEncoder.gas_usage(num_bytes.try_into().unwrap());

        // then
        assert_eq!(gas_usage, 4 * DATA_GAS_PER_BLOB);
    }

    #[test]
    fn test_chunk_size_zero() {
        // given
        let initial_range = 1..=10;
        let chunk_size = 0;

        // when
        let result = chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected: Vec<RangeInclusive<u64>> = vec![];
        assert_eq!(
            result, expected,
            "Expected empty vector when chunk_size is zero"
        );
    }

    #[test]
    fn test_chunk_size_larger_than_range() {
        // given
        let initial_range = 1..=5;
        let chunk_size = 10;

        // when
        let result = chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![1..=5];
        assert_eq!(
            result, expected,
            "Expected single chunk when chunk_size exceeds range length"
        );
    }

    #[test]
    fn test_exact_multiples() {
        // given
        let initial_range = 1..=10;
        let chunk_size = 2;

        // when
        let result = chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![1..=2, 3..=4, 5..=6, 7..=8, 9..=10];
        assert_eq!(result, expected, "Chunks should exactly divide the range");
    }

    #[test]
    fn test_non_exact_multiples() {
        // given
        let initial_range = 1..=10;
        let chunk_size = 3;

        // when
        let result = chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![1..=3, 4..=6, 7..=9, 10..=10];
        assert_eq!(
            result, expected,
            "Last chunk should contain the remaining elements"
        );
    }

    #[test]
    fn test_single_element_range() {
        // given
        let initial_range = 5..=5;
        let chunk_size = 1;

        // when
        let result = chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![5..=5];
        assert_eq!(
            result, expected,
            "Single element range should return one chunk with that element"
        );
    }

    #[test]
    fn test_start_equals_end_with_large_chunk_size() {
        // given
        let initial_range = 100..=100;
        let chunk_size = 50;

        // when
        let result = chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![100..=100];
        assert_eq!(
            result, expected,
            "Single element range should return one chunk regardless of chunk_size"
        );
    }

    #[test]
    fn test_chunk_size_one() {
        // given
        let initial_range = 10..=15;
        let chunk_size = 1;

        // when
        let result = chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![10..=10, 11..=11, 12..=12, 13..=13, 14..=14, 15..=15];
        assert_eq!(
            result, expected,
            "Each number should be its own chunk when chunk_size is one"
        );
    }

    #[test]
    fn test_full_range_chunk() {
        // given
        let initial_range = 20..=30;
        let chunk_size = 11;

        // when
        let result = chunk_range_inclusive(initial_range, chunk_size);

        // then
        let expected = vec![20..=30];
        assert_eq!(
            result, expected,
            "Whole range should be a single chunk when chunk_size equals range size"
        );
    }

    #[test]
    fn test_unpack_fee_history_empty_base_fee() {
        // given
        let fees = FeeHistory {
            oldest_block: 100,
            base_fee_per_gas: vec![],
            base_fee_per_blob_gas: vec![],
            reward: Some(vec![]),
            ..Default::default()
        };

        // when
        let result = unpack_fee_history(fees);

        // then
        let expected: Vec<BlockFees> = vec![];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected empty vector when base_fee_per_gas is empty"
        );
    }

    #[test]
    fn test_unpack_fee_history_missing_rewards() {
        // given
        let fees = FeeHistory {
            oldest_block: 200,
            base_fee_per_gas: vec![100, 200],
            base_fee_per_blob_gas: vec![150, 250],
            reward: None,
            ..Default::default()
        };

        // when
        let result = unpack_fee_history(fees.clone());

        // then
        let expected_error = services::Error::Other(format!("missing rewards field: {:?}", fees));
        assert_eq!(
            result.unwrap_err(),
            expected_error,
            "Expected error due to missing rewards field"
        );
    }

    #[test]
    fn test_unpack_fee_history_discrepancy_in_lengths_base_fee_rewards() {
        // given
        let fees = FeeHistory {
            oldest_block: 300,
            base_fee_per_gas: vec![100, 200, 300],
            base_fee_per_blob_gas: vec![150, 250, 350],
            reward: Some(vec![vec![10]]), // Should have 2 rewards for 2 blocks
            ..Default::default()
        };

        // when
        let result = unpack_fee_history(fees.clone());

        // then
        let expected_error =
            services::Error::Other(format!("discrepancy in lengths of fee fields: {:?}", fees));
        assert_eq!(
            result.unwrap_err(),
            expected_error,
            "Expected error due to discrepancy in lengths of fee fields"
        );
    }

    #[test]
    fn test_unpack_fee_history_discrepancy_in_lengths_blob_gas() {
        // given
        let fees = FeeHistory {
            oldest_block: 400,
            base_fee_per_gas: vec![100, 200, 300],
            base_fee_per_blob_gas: vec![150, 250], // Should have 3 elements
            reward: Some(vec![vec![10], vec![20]]),
            ..Default::default()
        };

        // when
        let result = unpack_fee_history(fees.clone());

        // then
        let expected_error =
            services::Error::Other(format!("discrepancy in lengths of fee fields: {:?}", fees));
        assert_eq!(
            result.unwrap_err(),
            expected_error,
            "Expected error due to discrepancy in base_fee_per_blob_gas lengths"
        );
    }

    #[test]
    fn test_unpack_fee_history_empty_reward_percentile() {
        // given
        let fees = FeeHistory {
            oldest_block: 500,
            base_fee_per_gas: vec![100, 200],
            base_fee_per_blob_gas: vec![150, 250],
            reward: Some(vec![vec![]]), // Empty percentile
            ..Default::default()
        };

        // when
        let result = unpack_fee_history(fees.clone());

        // then
        let expected_error =
            services::Error::Other("should have had at least one reward percentile".to_string());
        assert_eq!(
            result.unwrap_err(),
            expected_error,
            "Expected error due to empty reward percentile"
        );
    }

    #[test]
    fn test_unpack_fee_history_single_block() {
        // given
        let fees = FeeHistory {
            oldest_block: 600,
            base_fee_per_gas: vec![100, 200], // number_of_blocks =1
            base_fee_per_blob_gas: vec![150, 250],
            reward: Some(vec![vec![10]]),
            ..Default::default()
        };

        // when
        let result = unpack_fee_history(fees);

        // then
        let expected = vec![BlockFees {
            height: 600,
            fees: Fees {
                base_fee_per_gas: 100,
                reward: 10,
                base_fee_per_blob_gas: 150,
            },
        }];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected one BlockFees entry for a single block"
        );
    }

    #[test]
    fn test_unpack_fee_history_multiple_blocks() {
        // given
        let fees = FeeHistory {
            oldest_block: 700,
            base_fee_per_gas: vec![100, 200, 300, 400], // number_of_blocks =3
            base_fee_per_blob_gas: vec![150, 250, 350, 450],
            reward: Some(vec![vec![10], vec![20], vec![30]]),
            ..Default::default()
        };

        // when
        let result = unpack_fee_history(fees);

        // then
        let expected = vec![
            BlockFees {
                height: 700,
                fees: Fees {
                    base_fee_per_gas: 100,
                    reward: 10,
                    base_fee_per_blob_gas: 150,
                },
            },
            BlockFees {
                height: 701,
                fees: Fees {
                    base_fee_per_gas: 200,
                    reward: 20,
                    base_fee_per_blob_gas: 250,
                },
            },
            BlockFees {
                height: 702,
                fees: Fees {
                    base_fee_per_gas: 300,
                    reward: 30,
                    base_fee_per_blob_gas: 350,
                },
            },
        ];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected three BlockFees entries for three blocks"
        );
    }

    #[test]
    fn test_unpack_fee_history_large_values() {
        // given
        let fees = FeeHistory {
            oldest_block: u64::MAX - 2,
            base_fee_per_gas: vec![u128::MAX - 2, u128::MAX - 1, u128::MAX],
            base_fee_per_blob_gas: vec![u128::MAX - 3, u128::MAX - 2, u128::MAX - 1],
            reward: Some(vec![vec![u128::MAX - 4], vec![u128::MAX - 3]]),
            ..Default::default()
        };

        // when
        let result = unpack_fee_history(fees.clone());

        // then
        let expected = vec![
            BlockFees {
                height: u64::MAX - 2,
                fees: Fees {
                    base_fee_per_gas: u128::MAX - 2,
                    reward: u128::MAX - 4,
                    base_fee_per_blob_gas: u128::MAX - 3,
                },
            },
            BlockFees {
                height: u64::MAX - 1,
                fees: Fees {
                    base_fee_per_gas: u128::MAX - 1,
                    reward: u128::MAX - 3,
                    base_fee_per_blob_gas: u128::MAX - 2,
                },
            },
        ];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected BlockFees entries with large u64 values"
        );
    }

    #[test]
    fn test_unpack_fee_history_full_range_chunk() {
        // given
        let fees = FeeHistory {
            oldest_block: 800,
            base_fee_per_gas: vec![500, 600, 700, 800, 900], // number_of_blocks =4
            base_fee_per_blob_gas: vec![550, 650, 750, 850, 950],
            reward: Some(vec![vec![50], vec![60], vec![70], vec![80]]),
            ..Default::default()
        };

        // when
        let result = unpack_fee_history(fees);

        // then
        let expected = vec![
            BlockFees {
                height: 800,
                fees: Fees {
                    base_fee_per_gas: 500,
                    reward: 50,
                    base_fee_per_blob_gas: 550,
                },
            },
            BlockFees {
                height: 801,
                fees: Fees {
                    base_fee_per_gas: 600,
                    reward: 60,
                    base_fee_per_blob_gas: 650,
                },
            },
            BlockFees {
                height: 802,
                fees: Fees {
                    base_fee_per_gas: 700,
                    reward: 70,
                    base_fee_per_blob_gas: 750,
                },
            },
            BlockFees {
                height: 803,
                fees: Fees {
                    base_fee_per_gas: 800,
                    reward: 80,
                    base_fee_per_blob_gas: 850,
                },
            },
        ];
        assert_eq!(
            result.unwrap(),
            expected,
            "Expected BlockFees entries matching the full range chunk"
        );
    }
}
