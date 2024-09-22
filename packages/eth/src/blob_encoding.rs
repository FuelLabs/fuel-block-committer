use std::num::NonZeroUsize;

use alloy::eips::eip4844::BYTES_PER_BLOB;
use itertools::izip;
use itertools::Itertools;
use ports::types::NonEmptyVec;

use alloy::{
    consensus::{BlobTransactionSidecar, SidecarBuilder, SimpleCoder},
    eips::eip4844::{
        self, DATA_GAS_PER_BLOB, FIELD_ELEMENTS_PER_BLOB, FIELD_ELEMENT_BYTES, MAX_BLOBS_PER_BLOCK,
        MAX_DATA_GAS_PER_BLOCK,
    },
};

/// Intrinsic gas cost of a eth transaction.
const BASE_TX_COST: u64 = 21_000;

#[derive(Debug, Clone, Copy)]
pub struct Eip4844BlobEncoder;

impl Eip4844BlobEncoder {
    #[cfg(feature = "test-helpers")]
    pub const FRAGMENT_SIZE: usize =
        FIELD_ELEMENTS_PER_BLOB as usize * FIELD_ELEMENT_BYTES as usize;

    pub(crate) fn decode(
        fragments: &NonEmptyVec<NonEmptyVec<u8>>,
    ) -> crate::error::Result<(BlobTransactionSidecar, NonZeroUsize)> {
        eprintln!("decoding fragments");
        let fragments: Vec<_> = fragments
            .inner()
            .iter()
            .inspect(|e| eprintln!("inspecting fragment: {:?}", e.len()))
            // .take(6)
            .inspect(|e| eprintln!("inspecting fragment after take: {:?}", e.len()))
            .map(|e| {
                eprintln!("about to give to decode: {:?}", e.len());

                SingleBlob::decode(e)
            })
            .try_collect()?;
        eprintln!("decoded");

        let fragments_num = NonZeroUsize::try_from(fragments.len()).expect("cannot be 0");

        Ok((merge_into_sidecar(fragments), fragments_num))
    }
}

impl ports::l1::FragmentEncoder for Eip4844BlobEncoder {
    fn encode(&self, data: NonEmptyVec<u8>) -> ports::l1::Result<NonEmptyVec<NonEmptyVec<u8>>> {
        let sidecar = SidecarBuilder::from_coder_and_data(SimpleCoder::default(), data.inner())
            .build()
            .map_err(|e| ports::l1::Error::Other(format!("failed to build sidecar: {:?}", e)))?;

        let single_blobs =
            split_sidecar(sidecar).map_err(|e| ports::l1::Error::Other(e.to_string()))?;

        Ok(single_blobs
            .into_iter()
            .map(|blob| blob.encode())
            .collect_vec()
            .try_into()
            .expect("cannot be empty"))
    }

    fn gas_usage(&self, num_bytes: NonZeroUsize) -> u64 {
        let num_bytes =
            u64::try_from(num_bytes.get()).expect("to not have more than u64::MAX of storage data");

        // Taken from the SimpleCoder impl
        let required_fe = num_bytes.div_ceil(31).saturating_add(1);

        let blob_num = required_fe.div_ceil(FIELD_ELEMENTS_PER_BLOB);

        blob_num.saturating_mul(DATA_GAS_PER_BLOB)
    }
}

struct SingleBlob {
    // needs to be heap allocated because it's large enough to cause a stack overflow
    data: Box<eip4844::Blob>,
    committment: eip4844::Bytes48,
    proof: eip4844::Bytes48,
}

impl SingleBlob {
    const SIZE: usize =
        eip4844::BYTES_PER_BLOB + eip4844::BYTES_PER_COMMITMENT + eip4844::BYTES_PER_PROOF;

    fn decode(bytes: &NonEmptyVec<u8>) -> crate::error::Result<Self> {
        let bytes: &[u8; Self::SIZE] = bytes.inner().as_slice().try_into().map_err(|_| {
            crate::error::Error::Other(format!(
                "Failed to decode blob: expected {} bytes, got {}",
                Self::SIZE,
                bytes.len().get()
            ))
        })?;

        let data = Box::new(bytes[..eip4844::BYTES_PER_BLOB].try_into().unwrap());
        let remaining_bytes = &bytes[eip4844::BYTES_PER_BLOB..];

        let committment: [u8; 48] = remaining_bytes[..eip4844::BYTES_PER_COMMITMENT]
            .try_into()
            .unwrap();
        let remaining_bytes = &remaining_bytes[eip4844::BYTES_PER_COMMITMENT..];

        let proof: [u8; 48] = remaining_bytes[..eip4844::BYTES_PER_PROOF]
            .try_into()
            .unwrap();

        Ok(Self {
            data,
            committment: committment.into(),
            proof: proof.into(),
        })
    }

    fn encode(&self) -> NonEmptyVec<u8> {
        let mut bytes = Vec::with_capacity(Self::SIZE);
        bytes.extend_from_slice(self.data.as_slice());
        bytes.extend_from_slice(self.committment.as_ref());
        bytes.extend_from_slice(self.proof.as_ref());
        NonEmptyVec::try_from(bytes).expect("cannot be empty")
    }
}

fn split_sidecar(sidecar: BlobTransactionSidecar) -> crate::error::Result<Vec<SingleBlob>> {
    if sidecar.blobs.len() != sidecar.commitments.len()
        || sidecar.blobs.len() != sidecar.proofs.len()
    {
        return Err(crate::error::Error::Other(
            "sidecar blobs, commitments, and proofs must be the same length".to_string(),
        ));
    }

    let single_blobs = izip!(sidecar.blobs, sidecar.commitments, sidecar.proofs)
        .map(|(data, committment, proof)| SingleBlob {
            data: Box::new(data),
            committment,
            proof,
        })
        .collect();

    Ok(single_blobs)
}

fn merge_into_sidecar(
    single_blobs: impl IntoIterator<Item = SingleBlob>,
) -> BlobTransactionSidecar {
    let mut blobs = vec![];
    let mut commitments = vec![];
    let mut proofs = vec![];

    for blob in single_blobs {
        blobs.push(blob.data.as_slice().try_into().unwrap());
        commitments.push(blob.committment);
        proofs.push(blob.proof);
    }

    BlobTransactionSidecar {
        blobs,
        commitments,
        proofs,
    }
}

#[cfg(test)]
mod tests {
    use alloy::consensus::{SidecarBuilder, SimpleCoder};
    use eip4844::BlobTransactionSidecar;
    use ports::l1::FragmentEncoder;
    use rand::{rngs::SmallRng, Rng, RngCore, SeedableRng};
    use test_case::test_case;

    use super::*;

    #[test_case(100,  1; "one blob")]
    #[test_case(129 * 1024,  2; "two blobs")]
    #[test_case(257 * 1024,  3; "three blobs")]
    #[test_case(385 * 1024,  4; "four blobs")]
    #[test_case(513 * 1024,  5; "five blobs")]
    #[test_case(740 * 1024,  6; "six blobs")]
    #[test_case(768 * 1024,  7; "seven blobs")]
    #[test_case(896 * 1024,  8; "eight blobs")]
    fn gas_usage_for_data_storage(num_bytes: usize, num_blobs: usize) {
        // given

        // when
        let usage = Eip4844BlobEncoder.gas_usage(num_bytes.try_into().unwrap());

        // then
        assert_eq!(
            usage,
            num_blobs as u64 * alloy::eips::eip4844::DATA_GAS_PER_BLOB
        );

        let mut rng = SmallRng::from_seed([0; 32]);
        let mut data = vec![0; num_bytes];
        rng.fill(&mut data[..]);

        let mut builder = SidecarBuilder::from_coder_and_capacity(SimpleCoder::default(), 0);
        builder.ingest(&data);

        assert_eq!(builder.build().unwrap().blobs.len(), num_blobs,);
    }

    #[test]
    fn splitting_fails_if_uneven_proofs() {
        let invalid_sidecar = BlobTransactionSidecar {
            blobs: vec![Default::default()],
            commitments: vec![Default::default()],
            proofs: vec![],
        };
        assert!(split_sidecar(invalid_sidecar).is_err());
    }

    #[test]
    fn splitting_fails_if_uneven_commitments() {
        let invalid_sidecar = BlobTransactionSidecar {
            blobs: vec![Default::default()],
            commitments: vec![],
            proofs: vec![Default::default()],
        };
        assert!(split_sidecar(invalid_sidecar).is_err());
    }

    #[test]
    fn splitting_fails_if_uneven_blobs() {
        let invalid_sidecar = BlobTransactionSidecar {
            blobs: vec![],
            commitments: vec![Default::default()],
            proofs: vec![Default::default()],
        };
        assert!(split_sidecar(invalid_sidecar).is_err());
    }

    #[test]
    fn decoding_fails_if_extra_bytes_present() {
        let data = NonEmptyVec::try_from(vec![0; SingleBlob::SIZE + 1]).unwrap();

        assert!(SingleBlob::decode(&data).is_err());
    }

    #[test]
    fn decoding_fails_if_bytes_missing() {
        let data = NonEmptyVec::try_from(vec![0; SingleBlob::SIZE - 1]).unwrap();

        assert!(SingleBlob::decode(&data).is_err());
    }

    #[test]
    fn roundtrip_split_encode_decode_merge() {
        let mut random_data = vec![0; 1000];
        let mut rng = rand::rngs::SmallRng::from_seed([0; 32]);
        rng.fill_bytes(&mut random_data);

        let sidecar = SidecarBuilder::from_coder_and_data(SimpleCoder::default(), &random_data)
            .build()
            .unwrap();

        let single_blobs = split_sidecar(sidecar.clone()).unwrap();

        let fragments = single_blobs
            .into_iter()
            .map(|blob| blob.encode())
            .collect::<Vec<_>>();

        let reassmbled_single_blobs = fragments
            .into_iter()
            .map(|fragment| SingleBlob::decode(&fragment).unwrap())
            .collect_vec();

        let reassmbled_sidecar = merge_into_sidecar(reassmbled_single_blobs);

        assert_eq!(sidecar, reassmbled_sidecar);
    }
}
