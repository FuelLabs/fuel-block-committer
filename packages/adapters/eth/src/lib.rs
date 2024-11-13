use std::num::{NonZeroU32, NonZeroUsize};

use alloy::{
    consensus::BlobTransactionSidecar,
    eips::eip4844::{BYTES_PER_BLOB, DATA_GAS_PER_BLOB},
    primitives::U256,
};
use delegate::delegate;
use itertools::{izip, Itertools};
use services::{
    ports::l1::{Api, Contract, FragmentsSubmitted, Result},
    types::{
        BlockSubmissionTx, Fragment, L1Height, L1Tx, NonEmpty, NonNegative, TransactionResponse,
    },
};

mod aws;
mod error;
mod metrics;
mod websocket;

pub use alloy::primitives::Address;
pub use aws::*;
use fuel_block_committer_encoding::blob::{self, generate_sidecar};
pub use websocket::{KmsKeys, TxConfig, WebsocketClient};

impl Contract for WebsocketClient {
    delegate! {
        to self {
            async fn submit(&self, hash: [u8; 32], height: u32) -> Result<BlockSubmissionTx>;
            fn commit_interval(&self) -> NonZeroU32;
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct BlobEncoder;

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

impl services::ports::l1::FragmentEncoder for BlobEncoder {
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

impl Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn submit_state_fragments(
                &self,
                fragments: NonEmpty<Fragment>,
                previous_tx: Option<services::types::L1Tx>,
            ) -> Result<(L1Tx, FragmentsSubmitted)>;
            async fn balance(&self, address: Address) -> Result<U256>;
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

#[cfg(test)]
mod test {
    use alloy::eips::eip4844::DATA_GAS_PER_BLOB;
    use fuel_block_committer_encoding::blob;
    use services::ports::l1::FragmentEncoder;

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
