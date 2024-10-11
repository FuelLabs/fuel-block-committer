use std::num::NonZeroUsize;

use alloy::{
    consensus::BlobTransactionSidecar,
    eips::eip4844::{
        Blob, Bytes48, BYTES_PER_BLOB, BYTES_PER_COMMITMENT, BYTES_PER_PROOF, DATA_GAS_PER_BLOB,
        FIELD_ELEMENTS_PER_BLOB,
    },
};
use itertools::{izip, Itertools};
use ports::types::{CollectNonEmpty, Fragment, NonEmpty};

// Until the issue is fixed be careful that we use the `SidecarBuilder` and `SimpleCoder` from
// `copied_from_alloy`, there is a unit test that should protect against accidental import from the
// original location.
use super::copied_from_alloy::{SidecarBuilder, SimpleCoder};

#[derive(Debug, Clone, Copy)]
pub struct Eip4844BlobEncoder;

impl Eip4844BlobEncoder {
    #[cfg(feature = "test-helpers")]
    pub const FRAGMENT_SIZE: usize =
        FIELD_ELEMENTS_PER_BLOB as usize * alloy::eips::eip4844::FIELD_ELEMENT_BYTES as usize;

    pub(crate) fn decode(
        fragments: impl IntoIterator<Item = Fragment>,
    ) -> crate::error::Result<BlobTransactionSidecar> {
        let fragments: Vec<_> = fragments
            .into_iter()
            .map(SingleBlob::decode)
            .try_collect()?;

        Ok(merge_into_sidecar(fragments))
    }
}

impl ports::l1::FragmentEncoder for Eip4844BlobEncoder {
    fn encode(&self, data: NonEmpty<u8>) -> ports::l1::Result<NonEmpty<Fragment>> {
        let fragments = encode_into_blobs(data)
            .map_err(|e| ports::l1::Error::Other(e.to_string()))?
            .into_iter()
            .map(|blob| blob.as_fragment())
            .collect_nonempty()
            .expect("cannot be empty");

        Ok(fragments)
    }

    fn gas_usage(&self, num_bytes: NonZeroUsize) -> u64 {
        let num_bytes = u64::try_from(num_bytes.get()).unwrap_or(u64::MAX);

        // Taken from the SimpleCoder impl
        let required_fe = num_bytes.div_ceil(31).saturating_add(1);

        let blob_num = required_fe.div_ceil(FIELD_ELEMENTS_PER_BLOB);

        blob_num.saturating_mul(DATA_GAS_PER_BLOB)
    }
}

struct SingleBlob {
    // needs to be heap allocated because it's large enough to cause a stack overflow
    blobs: Box<Blob>,
    commitment: Bytes48,
    proof: Bytes48,
    unused_bytes: u32,
}

impl SingleBlob {
    const SIZE: usize = BYTES_PER_BLOB + BYTES_PER_COMMITMENT + BYTES_PER_PROOF;

    fn decode(fragment: Fragment) -> crate::error::Result<Self> {
        let data = Vec::from(fragment.data);
        let bytes: &[u8; Self::SIZE] = data.as_slice().try_into().map_err(|_| {
            crate::error::Error::Other(format!(
                "Failed to decode blob: expected {} bytes, got {}",
                Self::SIZE,
                data.len()
            ))
        })?;

        let len_checked = "checked earlier that enough bytes are available";

        let blobs = Box::new(bytes[..BYTES_PER_BLOB].try_into().expect(len_checked));
        let remaining_bytes = &bytes[BYTES_PER_BLOB..];

        let commitment: [u8; BYTES_PER_COMMITMENT] = remaining_bytes[..BYTES_PER_COMMITMENT]
            .try_into()
            .expect(len_checked);
        let remaining_bytes = &remaining_bytes[BYTES_PER_COMMITMENT..];

        let proof: [u8; BYTES_PER_COMMITMENT] = remaining_bytes[..BYTES_PER_PROOF]
            .try_into()
            .expect(len_checked);

        Ok(Self {
            blobs,
            commitment: commitment.into(),
            proof: proof.into(),
            unused_bytes: fragment.unused_bytes,
        })
    }

    fn as_fragment(&self) -> Fragment {
        let mut bytes = Vec::with_capacity(Self::SIZE);
        bytes.extend_from_slice(self.blobs.as_slice());
        bytes.extend_from_slice(self.commitment.as_ref());
        bytes.extend_from_slice(self.proof.as_ref());
        let data = NonEmpty::from_vec(bytes).expect("cannot be empty");

        Fragment {
            data,
            unused_bytes: self.unused_bytes,
            total_bytes: (BYTES_PER_BLOB as u32).try_into().expect("not zero"),
        }
    }
}

fn encode_into_blobs(data: NonEmpty<u8>) -> crate::error::Result<NonEmpty<SingleBlob>> {
    let builder = SidecarBuilder::<SimpleCoder>::from_coder_and_data(
        SimpleCoder::default(),
        &Vec::from(data),
    );
    let total_used_bytes = u32::try_from(builder.len())
        .map_err(|_| {
            crate::error::Error::Other("cannot handle more than u32::MAX bytes".to_string())
        })?
        .saturating_sub(builder.unused_bytes_in_last_fe() as u32);

    let sidecar = builder
        .build()
        .map_err(|e| crate::error::Error::Other(e.to_string()))?;

    let num_blobs = u32::try_from(sidecar.blobs.len()).map_err(|_| {
        crate::error::Error::Other("cannot handle more than u32::MAX blobs".to_string())
    })?;

    if num_blobs == 0 {
        return Err(crate::error::Error::Other("no blobs to split".to_string()));
    }

    let remainder = total_used_bytes % BYTES_PER_BLOB as u32;
    let unused_data_in_last_blob = if remainder == 0 {
        0
    } else {
        BYTES_PER_BLOB as u32 - remainder
    };

    // blobs not consumed here because that would place them on the stack at some point. the aloy
    // type being huge it then causes a stack overflow
    let single_blobs = izip!(&sidecar.blobs, sidecar.commitments, sidecar.proofs)
        .enumerate()
        .map(|(index, (data, commitment, proof))| {
            let index = u32::try_from(index)
                .expect("checked earlier there are no more than u32::MAX blobs");

            let unused_data = if index == num_blobs.saturating_sub(1) {
                unused_data_in_last_blob
            } else {
                0
            };

            SingleBlob {
                blobs: Box::new(data.as_slice().try_into().expect("number of bytes match")),
                commitment,
                proof,
                unused_bytes: unused_data,
            }
        })
        .collect_nonempty()
        .expect("checked is not empty");

    Ok(single_blobs)
}

fn merge_into_sidecar(
    single_blobs: impl IntoIterator<Item = SingleBlob>,
) -> BlobTransactionSidecar {
    let mut blobs = vec![];
    let mut commitments = vec![];
    let mut proofs = vec![];

    for blob in single_blobs {
        blobs.push(*blob.blobs);
        commitments.push(blob.commitment);
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

    use ports::l1::FragmentEncoder;
    use proptest::prop_assert_eq;
    use rand::{rngs::SmallRng, Rng, RngCore, SeedableRng};
    use test_case::test_case;

    use super::*;
    use crate::blob_encoding::copied_from_alloy::SidecarCoder;

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

    fn non_empty_rand_data(amount: usize) -> NonEmpty<u8> {
        if amount == 0 {
            panic!("cannot create empty data");
        } else {
            let mut rng = SmallRng::from_seed([0; 32]);
            let mut data = vec![0; amount];
            rng.fill(&mut data[..]);
            NonEmpty::collect(data).unwrap()
        }
    }

    #[test]
    fn roundtrip_split_encode_decode_merge() {
        let random_data = non_empty_rand_data(110_000);

        let single_blobs = encode_into_blobs(random_data.clone()).unwrap();

        let merged_sidecar = merge_into_sidecar(single_blobs);

        let should_be_original_data = SimpleCoder::default()
            .decode_all(&merged_sidecar.blobs)
            .unwrap()
            .into_iter()
            .flatten()
            .collect_nonempty()
            .unwrap();

        assert_eq!(should_be_original_data, random_data);
    }

    use proptest::prelude::*;
    proptest::proptest! {
        // // You maybe want to make this limit bigger when changing the code
        #![proptest_config(ProptestConfig { cases: 10, .. ProptestConfig::default() })]
        #[test]
        fn alloy_blob_encoding_issue_regression(amount in 1..=DATA_GAS_PER_BLOB*20) {
            // given
            let encoder = Eip4844BlobEncoder;
            let mut rng = SmallRng::from_seed([0; 32]);
            let mut data = vec![0; amount as usize];
            rng.fill_bytes(&mut data[..]);

            // when
            let fragments = encoder
                .encode(NonEmpty::from_vec(data.clone()).unwrap())
                .map_err(|e| {
                    crate::error::Error::Other(format!("cannot encode {amount}B : {}", e))
                })?;

            // then
            let sidecar = Eip4844BlobEncoder::decode(fragments).unwrap();

            let mut builder = SidecarBuilder::<SimpleCoder>::new();
            // TODO: ingest more at once
            for byte in &data {
                builder.ingest(std::slice::from_ref(byte));
            }

            let decoded_data = SimpleCoder::default()
                .decode_all(&sidecar.blobs)
                .ok_or_else(|| {
                    crate::error::Error::Other(format!("cannot decode blobs for amount {amount}",))
                })?
                .into_iter()
                .flatten()
                .collect_vec();

            prop_assert_eq!(data, decoded_data);
        }
    }
    proptest::proptest! {
        // // You maybe want to make this limit bigger when changing the code
        #![proptest_config(ProptestConfig { cases: 10, .. ProptestConfig::default() })]
        #[test]
        fn shows_unused_bytes(num_bytes in 1..=DATA_GAS_PER_BLOB*6) {
            // given
            let mut data = non_empty_rand_data(num_bytes as usize);
            // because of our assertion of zeroes at end
            *data.last_mut() = 1;


            // when
            let single_blobs = encode_into_blobs(data).unwrap();

            // then
            let num_blobs = single_blobs.len();

            for blob in single_blobs.iter().take(num_blobs - 1) {
                prop_assert_eq!(blob.unused_bytes, 0);
            }

            let last_blob = single_blobs.last();
            let unused_bytes = last_blob.unused_bytes as usize;

            // a hacky way to validate but good enough when input data is random
            let zeroes_at_end = last_blob
                .blobs
                .iter()
                .rev()
                .take_while(|byte| **byte == 0)
                .count();

            prop_assert_eq!(zeroes_at_end, unused_bytes);
        }
    }
}
