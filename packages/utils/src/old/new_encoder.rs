use alloy::eips::eip4844::{DATA_GAS_PER_BLOB, USABLE_BYTES_PER_BLOB};
use ports::{l1::FragmentEncoder, types::CollectNonEmpty};

use crate::Result;

use super::{
    copied_from_alloy::{SidecarCoder, SimpleCoder},
    encoder::{merge_into_sidecar, SingleBlob},
    Eip4844BlobEncoder,
};

pub struct NewEncoder {}

pub struct NewDecoder {}

#[derive(Debug, Copy, Clone, PartialEq)]
struct BlobHeader {
    size: u32,
    id: u32,
    is_last: bool,
    idx: u32,
}

impl BlobHeader {
    const ENCODED_NUM_BITS: usize = 64;
}

impl NewDecoder {
    pub fn decode(&self, blobs: Vec<SingleBlob>) -> Result<ports::types::NonEmpty<u8>> {
        let sidecar = merge_into_sidecar(blobs);
        Ok(SimpleCoder::default()
            .decode_all(&sidecar.blobs)
            .unwrap()
            .into_iter()
            .flatten()
            .collect_nonempty()
            .unwrap())
    }
}

impl NewEncoder {
    pub fn blobs_needed_to_encode(&self, num_bytes: usize) -> usize {
        const USABLE_BITS_PER_BLOB: usize = USABLE_BYTES_PER_BLOB.saturating_mul(8);

        let num_bits = num_bytes.saturating_mul(8);

        let with_header = num_bits.saturating_add(BlobHeader::ENCODED_NUM_BITS);

        with_header.div_ceil(USABLE_BITS_PER_BLOB)
    }

    pub fn encode(&self, data: Vec<u8>, id: u32) -> Result<Vec<SingleBlob>> {
        let blobs_needed = self.blobs_needed_to_encode(data.len());
        let sidecar = SimpleCoder::default().encode_all(data);
        let mut blobs = Vec::with_capacity(blobs_needed);
        for (idx, blob) in sidecar.into_iter().enumerate() {
            let header = BlobHeader {
                size: data.len() as u32,
                id,
                is_last: idx == blobs_needed - 1,
                idx: idx as u32,
            };
            blobs.push(SingleBlob::new(header, blob));
        }
        Ok(blobs)
    }
}

impl FragmentEncoder for NewEncoder {
    fn encode(
        &self,
        data: ports::types::NonEmpty<u8>,
        id: ports::types::NonNegative<i32>,
    ) -> Result<ports::types::NonEmpty<ports::types::Fragment>> {
    }

    fn gas_usage(&self, num_bytes: std::num::NonZeroUsize) -> u64 {
        let num_blobs = self.blobs_needed_to_encode(num_bytes.get()) as u64;

        num_blobs.saturating_mul(DATA_GAS_PER_BLOB)
    }
}

#[cfg(test)]
mod test {
    use alloy::consensus::EnvKzgSettings;
    use itertools::Itertools;
    use ports::{l1::FragmentEncoder, types::NonEmpty};
    use rand::{rngs::SmallRng, seq::SliceRandom, RngCore, SeedableRng};
    use test_case::test_case;

    use crate::blob_encoding::{
        encoder::{merge_into_sidecar, SingleBlob},
        new_encoder::NewEncoder,
    };

    #[test_case(1,  1; "one blob")]
    #[test_case(130040,  1; "one blob limit")]
    #[test_case(130041,  2; "two blobs")]
    #[test_case(130040 * 2,  2; "two blobs limit")]
    #[test_case(130040 * 2 + 1,  2; "three blobs")]
    #[test_case(130040 * 3,  3; "three blobs limit")]
    #[test_case(130040 * 3 + 1,  3; "four blobs")]
    #[test_case(130040 * 4,  4; "four blobs limit")]
    #[test_case(130040 * 4 + 1,  4; "five blobs")]
    #[test_case(130040 * 5,  5; "five blobs limit")]
    #[test_case(130040 * 5 + 1,  5; "six blobs")]
    #[test_case(130040 * 6,  6; "six blobs limit")]
    #[test_case(130040 * 6 + 1,  6; "seven blobs")]
    fn gas_usage_for_data_storage(num_bytes: usize, num_blobs: usize) {
        // given
        let encoder = NewEncoder {};

        // when
        let usage = encoder.gas_usage(num_bytes.try_into().unwrap());

        // then
        assert_eq!(
            usage,
            num_blobs as u64 * alloy::eips::eip4844::DATA_GAS_PER_BLOB
        );
    }

    #[test_case(1)]
    #[test_case(200_000)]
    #[test_case(600_000)]
    #[test_case(1_200_000)]
    fn encoded_fragments_can_be_turned_into_valid_blobs(byte_num: usize) {
        // given
        let encoder = NewEncoder {};

        let mut data = vec![0; byte_num];
        let mut rng = SmallRng::from_seed([0; 32]);
        rng.fill_bytes(&mut data[..]);
        let data = NonEmpty::from_vec(data).unwrap();

        // when
        let fragments = encoder.encode(data.clone(), 0.into()).unwrap();

        // then
        for fragment in fragments {
            let blob = SingleBlob::decode(fragment).unwrap();

            let sidecar = merge_into_sidecar([blob]);
            let versioned_hashes = sidecar.versioned_hashes().collect_vec();
            sidecar
                .validate(&versioned_hashes, EnvKzgSettings::default().get())
                .unwrap();
        }
    }

    #[test_case(1)]
    #[test_case(200_000)]
    #[test_case(600_000)]
    #[test_case(1_200_000)]
    fn fragments_can_be_decoded_when_in_order(byte_num: usize) {
        // given
        let encoder = NewEncoder {};

        let mut data = vec![0; byte_num];
        let mut rng = SmallRng::from_seed([0; 32]);
        rng.fill_bytes(&mut data[..]);
        let data = NonEmpty::from_vec(data).unwrap();
        let fragments = encoder.encode(data.clone(), 0.into()).unwrap();

        // when
        let decoded_data = encoder.decode(&fragments).unwrap();

        // then
        assert_eq!(decoded_data, data);
    }

    #[test_case(1)]
    #[test_case(200_000)]
    #[test_case(600_000)]
    #[test_case(1_200_000)]
    fn fragments_can_be_decoded_even_if_shuffled_around(byte_num: usize) {
        // given
        let encoder = NewEncoder {};

        let mut rng = SmallRng::from_seed([0; 32]);
        let data = {
            let mut data = vec![0; byte_num];
            rng.fill_bytes(&mut data[..]);
            NonEmpty::from_vec(data).unwrap()
        };

        let fragments = {
            let mut fragments = Vec::from(encoder.encode(data.clone(), 0.into()).unwrap());
            fragments.shuffle(&mut rng);
            NonEmpty::from_vec(fragments).unwrap()
        };

        // when
        let decoded_data = encoder.decode(&fragments).unwrap();

        // then
        assert_eq!(decoded_data, data);
    }

    #[test]
    fn can_decode_blob_header() {
        // given
        let encoder = NewEncoder {};

        let data = NonEmpty::from_vec(vec![0; 100]).unwrap();

        let fragments = encoder.encode(data.clone(), 0.into()).unwrap();
    }
}
