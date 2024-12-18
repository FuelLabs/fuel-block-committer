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
    fee_tracker::port::l1::SequentialBlockFees,
    types::{
        BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Height, L1Tx, NonEmpty, NonNegative,
        TransactionResponse,
    },
    Result,
};

mod aws;
mod error;
mod fee_conversion;
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

impl services::fee_tracker::port::l1::Api for WebsocketClient {
    async fn current_height(&self) -> Result<u64> {
        self._get_block_number().await
    }

    async fn fees(&self, height_range: RangeInclusive<u64>) -> Result<SequentialBlockFees> {
        const REWARD_PERCENTILE: f64 =
            alloy::providers::utils::EIP1559_FEE_ESTIMATION_REWARD_PERCENTILE;
        // so that a alloy version bump doesn't surprise us
        const_assert!(REWARD_PERCENTILE == 20.0,);

        // There is a comment in alloy about not doing more than 1024 blocks at a time
        const RPC_LIMIT: u64 = 1024;

        let fees: Vec<FeeHistory> = stream::iter(fee_conversion::chunk_range_inclusive(
            height_range,
            RPC_LIMIT,
        ))
        .then(|range| self.fees(range, std::slice::from_ref(&REWARD_PERCENTILE)))
        .try_collect()
        .await?;

        let mut unpacked_fees = vec![];
        for fee in fees {
            unpacked_fees.extend(fee_conversion::unpack_fee_history(fee)?);
        }

        unpacked_fees
            .try_into()
            .map_err(|e| services::Error::Other(format!("{e}")))
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

#[cfg(test)]
mod test {
    use alloy::eips::eip4844::DATA_GAS_PER_BLOB;
    use fuel_block_committer_encoding::blob;
    use services::block_bundler::port::l1::FragmentEncoder;

    use crate::BlobEncoder;

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
}
