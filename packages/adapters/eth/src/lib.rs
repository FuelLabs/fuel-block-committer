use std::{
    cmp::min,
    num::{NonZeroU32, NonZeroUsize},
    ops::RangeInclusive,
    time::Duration,
};

use alloy::{
    consensus::BlobTransactionSidecar,
    eips::eip4844::{BYTES_PER_BLOB, DATA_GAS_PER_BLOB},
    primitives::U256,
    providers::utils::EIP1559_FEE_ESTIMATION_REWARD_PERCENTILE,
};
use delegate::delegate;
use itertools::{izip, zip, Itertools};
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
    async fn fees(&self, height_range: RangeInclusive<u64>) -> SequentialBlockFees {
        const REWARD_PERCENTILE: f64 =
            alloy::providers::utils::EIP1559_FEE_ESTIMATION_REWARD_PERCENTILE;

        // so that a alloy version bump doesn't surprise us
        const_assert!(REWARD_PERCENTILE == 20.0,);

        let mut fees = vec![];

        // TODO: segfault see when this can be None
        // TODO: check edgecases
        let mut current_height = height_range.clone().min().unwrap();
        while current_height <= *height_range.end() {
            // There is a comment in alloy about not doing more than 1024 blocks at a time
            const RPC_LIMIT: u64 = 1024;

            let upper_bound = min(
                current_height.saturating_add(RPC_LIMIT).saturating_sub(1),
                *height_range.end(),
            );

            let history = self
                .fees(
                    current_height..=upper_bound,
                    std::slice::from_ref(&REWARD_PERCENTILE),
                )
                .await
                .unwrap();

            assert_eq!(
                history.reward.as_ref().unwrap().len(),
                (current_height..=upper_bound).count()
            );

            fees.push(history);

            current_height = upper_bound.saturating_add(1);
        }

        let new_fees = fees
            .into_iter()
            .flat_map(|fees| {
                // TODO: segfault check if the vector is ever going to have less than 2 elements, maybe
                // for block count 0?
                // eprintln!("received {fees:?}");
                let number_of_blocks = fees.base_fee_per_blob_gas.len().checked_sub(1).unwrap();
                let rewards = fees
                    .reward
                    .unwrap()
                    .into_iter()
                    .map(|mut perc| perc.pop().unwrap())
                    .collect_vec();

                let oldest_block = fees.oldest_block;

                debug_assert_eq!(rewards.len(), number_of_blocks);

                izip!(
                    (oldest_block..),
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
            })
            .collect_vec();

        // eprintln!("converted into {new_fees:?}");

        new_fees.try_into().unwrap()
    }

    async fn current_block_height(&self) -> u64 {
        self._get_block_number().await.unwrap()
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use alloy::eips::eip4844::DATA_GAS_PER_BLOB;
    use fuel_block_committer_encoding::blob;
    use services::{
        block_bundler::port::l1::FragmentEncoder, block_committer::port::l1::Api,
        fee_analytics::port::l1::FeesProvider,
    };

    use crate::{BlobEncoder, Signer, Signers};

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

    // #[tokio::test]
    // async fn can_connect_to_eth_mainnet() {
    //     let current_height = client._get_block_number().await.unwrap();
    //
    //     let fees = FeesProvider::fees(&client, current_height - 1026..=current_height).await;
    //
    //     panic!("{:?}", fees);
    // }
}
