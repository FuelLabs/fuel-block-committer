use std::num::NonZeroUsize;

use alloy::{
    consensus::{BlobTransactionSidecar, SidecarBuilder, SimpleCoder},
    eips::eip4844::{self, BYTES_PER_BLOB, DATA_GAS_PER_BLOB, FIELD_ELEMENTS_PER_BLOB},
};
use itertools::{izip, Itertools};
use ports::types::{CollectNonEmpty, Fragment, NonEmpty};

#[derive(Debug, Clone, Copy)]
pub struct Eip4844BlobEncoder;

impl Eip4844BlobEncoder {
    #[cfg(feature = "test-helpers")]
    pub const FRAGMENT_SIZE: usize =
        FIELD_ELEMENTS_PER_BLOB as usize * eip4844::FIELD_ELEMENT_BYTES as usize;

    pub(crate) fn decode(
        fragments: NonEmpty<Fragment>,
    ) -> crate::error::Result<(BlobTransactionSidecar, NonZeroUsize)> {
        let fragments: Vec<_> = fragments
            .into_iter()
            .take(6)
            .map(SingleBlob::decode)
            .try_collect()?;

        let fragments_num = NonZeroUsize::try_from(fragments.len()).expect("cannot be 0");

        Ok((merge_into_sidecar(fragments), fragments_num))
    }
}

impl ports::l1::FragmentEncoder for Eip4844BlobEncoder {
    fn encode(&self, data: NonEmpty<u8>) -> ports::l1::Result<NonEmpty<Fragment>> {
        let builder =
            SidecarBuilder::from_coder_and_data(SimpleCoder::default(), Vec::from(data).as_slice());

        let single_blobs =
            split_sidecar(builder).map_err(|e| ports::l1::Error::Other(e.to_string()))?;

        Ok(single_blobs
            .into_iter()
            .map(|blob| blob.encode())
            .collect_nonempty()
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
    unused_bytes: u32,
}

impl SingleBlob {
    const SIZE: usize =
        eip4844::BYTES_PER_BLOB + eip4844::BYTES_PER_COMMITMENT + eip4844::BYTES_PER_PROOF;

    fn decode(fragment: Fragment) -> crate::error::Result<Self> {
        let data = Vec::from(fragment.data);
        let bytes: &[u8; Self::SIZE] = data.as_slice().try_into().map_err(|_| {
            crate::error::Error::Other(format!(
                "Failed to decode blob: expected {} bytes, got {}",
                Self::SIZE,
                data.len()
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
            unused_bytes: fragment.unused_bytes,
        })
    }

    fn encode(&self) -> Fragment {
        let mut bytes = Vec::with_capacity(Self::SIZE);
        bytes.extend_from_slice(self.data.as_slice());
        bytes.extend_from_slice(self.committment.as_ref());
        bytes.extend_from_slice(self.proof.as_ref());
        let data = NonEmpty::collect(bytes).expect("cannot be empty");

        Fragment {
            data,
            unused_bytes: self.unused_bytes,
            total_bytes: (BYTES_PER_BLOB as u32).try_into().expect("not zero"),
        }
    }
}

fn split_sidecar(builder: SidecarBuilder) -> crate::error::Result<Vec<SingleBlob>> {
    let num_bytes = u32::try_from(builder.len()).map_err(|_| {
        crate::error::Error::Other("cannot handle more than u32::MAX bytes".to_string())
    })?;
    let sidecar = builder.build()?;

    if sidecar.blobs.len() != sidecar.commitments.len()
        || sidecar.blobs.len() != sidecar.proofs.len()
    {
        return Err(crate::error::Error::Other(
            "sidecar blobs, commitments, and proofs must be the same length".to_string(),
        ));
    }

    let num_blobs = u32::try_from(sidecar.blobs.len()).map_err(|_| {
        crate::error::Error::Other("cannot handle more than u32::MAX blobs".to_string())
    })?;

    let unused_data_in_last_blob =
        (BYTES_PER_BLOB as u32).saturating_sub(num_bytes % BYTES_PER_BLOB as u32);

    let single_blobs = izip!(sidecar.blobs, sidecar.commitments, sidecar.proofs)
        .enumerate()
        .map(|(index, (data, committment, proof))| {
            let index = u32::try_from(index)
                .expect("checked earlier there are no more than u32::MAX blobs");

            let unused_data = if index == num_blobs.saturating_sub(1) {
                unused_data_in_last_blob
            } else {
                0
            };

            SingleBlob {
                data: Box::new(data),
                committment,
                proof,
                unused_bytes: unused_data,
            }
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
    fn decoding_fails_if_extra_bytes_present() {
        let data = Fragment {
            data: NonEmpty::collect(vec![0; SingleBlob::SIZE + 1]).unwrap(),
            unused_bytes: 0,
            total_bytes: 1.try_into().unwrap(),
        };

        assert!(SingleBlob::decode(data).is_err());
    }

    #[test]
    fn decoding_fails_if_bytes_missing() {
        let data = Fragment {
            data: NonEmpty::collect(vec![0; SingleBlob::SIZE - 1]).unwrap(),
            unused_bytes: 0,
            total_bytes: 1.try_into().unwrap(),
        };

        assert!(SingleBlob::decode(data).is_err());
    }

    #[test]
    fn roundtrip_split_encode_decode_merge() {
        let mut random_data = vec![0; 1000];
        let mut rng = rand::rngs::SmallRng::from_seed([0; 32]);
        rng.fill_bytes(&mut random_data);

        let sidecar = SidecarBuilder::from_coder_and_data(SimpleCoder::default(), &random_data);

        let single_blobs = split_sidecar(sidecar.clone()).unwrap();

        let fragments = single_blobs
            .into_iter()
            .map(|blob| blob.encode())
            .collect::<Vec<_>>();

        let reassmbled_single_blobs = fragments
            .into_iter()
            .map(|fragment| SingleBlob::decode(fragment).unwrap())
            .collect_vec();

        let reassmbled_sidecar = merge_into_sidecar(reassmbled_single_blobs);

        assert_eq!(sidecar.build().unwrap(), reassmbled_sidecar);
    }

    #[test]
    fn shows_unused_bytes() {
        let mut random_data = vec![0; 1000];
        let mut rng = rand::rngs::SmallRng::from_seed([0; 32]);
        rng.fill_bytes(&mut random_data);

        let sidecar = SidecarBuilder::from_coder_and_data(SimpleCoder::default(), &random_data);

        let single_blobs = split_sidecar(sidecar.clone()).unwrap();

        assert_eq!(single_blobs.len(), 1);
        assert_eq!(single_blobs[0].unused_bytes, 129984);
    }
}
