use std::num::{NonZeroU32, NonZeroUsize};

use alloy::{consensus::BlobTransactionSidecar, eips::eip4844::BYTES_PER_BLOB, primitives::U256};
use delegate::delegate;
use itertools::{izip, Itertools};
use ports::{
    l1::{Api, Contract, FragmentsSubmitted, Result},
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

    pub fn sidecar_from_fragments(
        fragments: impl IntoIterator<Item = Fragment>,
    ) -> crate::Result<BlobTransactionSidecar> {
        let mut sidecar = BlobTransactionSidecar::default();

        for fragment in fragments {
            let data = Vec::from(fragment.data);

            sidecar.blobs.push(Default::default());
            sidecar.commitments.push(Default::default());
            sidecar.proofs.push(Default::default());

            let read_location = data.as_slice();

            sidecar.blobs.last_mut().unwrap()[..].copy_from_slice(&read_location[..BYTES_PER_BLOB]);
            let read_location = &read_location[BYTES_PER_BLOB..];

            sidecar
                .commitments
                .last_mut()
                .unwrap()
                .copy_from_slice(&read_location[..48]);
            let read_location = &read_location[48..];

            sidecar
                .proofs
                .last_mut()
                .unwrap()
                .copy_from_slice(&read_location[..48]);
        }

        Ok(sidecar)
    }
}

impl ports::l1::FragmentEncoder for BlobEncoder {
    fn encode(&self, data: NonEmpty<u8>, id: NonNegative<i32>) -> Result<NonEmpty<Fragment>> {
        let data = Vec::from(data);
        let blobs = blob::Encoder::new().encode(&data, id.as_u32()).unwrap();

        let bits_usage = blobs
            .iter()
            .map(|blob| {
                let blob::Header::V1(header) = blob::Decoder::default().read_header(blob).unwrap();
                header.num_bits
            })
            .collect_vec();

        let sidecar = generate_sidecar(blobs).unwrap();

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
                data: NonEmpty::from_vec(data_commitment_and_proof).unwrap(),
                unused_bytes: bits_per_blob.saturating_sub(used_bits).saturating_div(8),
                total_bytes: bits_per_blob.saturating_div(8).try_into().unwrap(),
            }
        })
        .collect();

        Ok(NonEmpty::from_vec(fragments).unwrap())
    }

    fn gas_usage(&self, num_bytes: NonZeroUsize) -> u64 {
        blob::Encoder::default().blobs_needed_to_encode(num_bytes.get()) as u64
    }
}

impl Api for WebsocketClient {
    delegate! {
        to (*self) {
            async fn submit_state_fragments(
                &self,
                fragments: NonEmpty<Fragment>,
                previous_tx: Option<ports::types::L1Tx>,
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
