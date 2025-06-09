#[cfg(test)]
mod tests {
    use alloy::{consensus::EnvKzgSettings, eips::eip4844::DATA_GAS_PER_BLOB};
    use bitvec::{order::Msb0, vec::BitVec};
    use fuel_block_committer_encoding::{
        blob::{self, generate_sidecar},
        bundle::{self, BundleEncoder, BundleV1},
    };
    use itertools::Itertools;
    use proptest::prelude::*;
    use rand::{rngs::SmallRng, seq::SliceRandom, RngCore, SeedableRng};
    use test_case::test_case;

    #[test_case(1,  1; "one blob")]
    #[test_case(130037,  1; "one blob limit")]
    #[test_case(130038,  2; "two blobs")]
    #[test_case(130037 * 2,  2; "two blobs limit")]
    #[test_case(130037 * 2  + 1,  3; "three blobs")]
    fn can_calculate_blobs_needed_without_encoding(num_bytes: usize, num_blobs: usize) {
        // given
        let encoder = blob::Encoder::default();

        // when
        let usage = encoder.blobs_needed_to_encode(num_bytes);

        // then
        assert_eq!(usage, num_blobs);
    }

    proptest::proptest! {
        // // You maybe want to make this limit bigger when changing the code
        #![proptest_config(ProptestConfig { cases: 10, .. ProptestConfig::default() })]
        #[test]
        fn calculated_blobs_needed_the_same_as_if_you_encode(byte_amount in 1..=DATA_GAS_PER_BLOB*20) {
            // given
            let encoder = blob::Encoder::default();

            // when
            let usage = encoder.blobs_needed_to_encode(byte_amount as usize);
            let actual_blob_num = encoder.encode(&vec![0; byte_amount as usize], 0).unwrap().len();

            // then
            proptest::prop_assert_eq!(usage, actual_blob_num);
        }
    }
    proptest::proptest! {
        // You maybe want to make this limit bigger when changing the code
        #![proptest_config(ProptestConfig { cases: 10, .. ProptestConfig::default() })]

        #[test]
        fn encode_decode_validate_roundtrip(block_count in 1u64..=3600, block_size in 1u64..=5555, bundle_id in 0u32..=u32::MAX) {
        // given
        let mut rng = SmallRng::from_seed([0; 32]);
        let blocks = std::iter::repeat_with(|| {
        let mut buf = vec![0; block_size as usize];
        rng.fill(&mut buf[..]);
        buf
    })
            .take(block_count as usize)
            .collect::<Vec<_>>();

        let blocks = bundle::Bundle::V1(BundleV1{blocks });

        let blocks_encoded = bundle::Encoder::default().encode(blocks.clone()).unwrap();

        // we shuffle them around
        let blobs = {
            let mut blobs = blob::Encoder::default().encode(&blocks_encoded, bundle_id).unwrap();
            blobs.shuffle(&mut rng);
            blobs
        };

        // blobs are valid
        for blob in blobs.clone() {
            let sidecar = generate_sidecar(vec![blob]).unwrap();
            let versioned_hashes = sidecar.versioned_hashes().collect_vec();
            sidecar
                .validate(&versioned_hashes, EnvKzgSettings::default().get())
                .unwrap();
        }

        // can be decoded into original data
        let decoded_blob_data  = blob::Decoder::default().decode(&blobs).unwrap();
        proptest::prop_assert_eq!(&decoded_blob_data, &blocks_encoded);

        let decoded_blocks = bundle::Decoder::default().decode(&decoded_blob_data).unwrap();
        proptest::prop_assert_eq!(decoded_blocks, blocks);
        }
    }

    #[test_case(1)]
    #[test_case(50)]
    #[test_case(200_000)]
    #[test_case(600_000)]
    #[test_case(1_200_000)]
    fn can_generate_valid_proofs_and_commitments_for_encoded_blobs(byte_num: usize) {
        // given
        let encoder = blob::Encoder::default();

        let mut data = vec![0; byte_num];
        let mut rng = SmallRng::from_seed([0; 32]);
        rng.fill_bytes(&mut data[..]);

        // when
        let blobs = encoder.encode(&data, 0).unwrap();

        // then
        for blob in blobs {
            let sidecar = generate_sidecar(vec![blob]).unwrap();
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
    fn blobs_can_be_decoded_when_in_order(byte_num: usize) {
        // given
        let encoder = blob::Encoder::default();

        let mut data = vec![0; byte_num];
        let mut rng = SmallRng::from_seed([0; 32]);
        rng.fill_bytes(&mut data[..]);
        let blobs = encoder.encode(&data, 55).unwrap();

        let decoder = blob::Decoder::default();

        // when
        let decoded_data = decoder.decode(&blobs).unwrap();

        // then
        assert_eq!(decoded_data, data);
    }

    #[test_case(1)]
    #[test_case(200_000)]
    #[test_case(600_000)]
    #[test_case(1_200_000)]
    fn blobs_can_be_decoded_even_if_shuffled_around(byte_num: usize) {
        // given
        let encoder = blob::Encoder::default();

        let mut rng = SmallRng::from_seed([0; 32]);
        let data = {
            let mut data = vec![0; byte_num];
            rng.fill_bytes(&mut data[..]);
            data
        };

        let blobs = {
            let mut blobs = encoder.encode(&data, 78032).unwrap();
            blobs.shuffle(&mut rng);
            blobs
        };

        let decoder = blob::Decoder::default();

        // when
        let decoded_data = decoder.decode(&blobs).unwrap();

        // then
        assert_eq!(decoded_data, data);
    }

    #[test_case(100, 0; "id 0")]
    #[test_case(100, 5; "normal case")]
    #[test_case(100, u32::MAX; "max id")]
    #[test_case(100, 26896; "prod example")]
    #[test_case(200_000, 26896; "2 blobs")]
    #[test_case(2_000_000, 8821; "many blobs")]
    fn roundtrip_blob_header(num_bytes: usize, bundle_id: u32) {
        // given
        let blobs = {
            let encoder = blob::Encoder::default();
            // we put all 1s so that we may use the appearance of a 0 as a marker for where the
            // data ends
            encoder.encode(&vec![255; num_bytes], bundle_id).unwrap()
        };

        let decoder = blob::Decoder::default();

        // when
        let headers: Vec<_> = blobs
            .iter()
            .map(|blob| decoder.read_header(blob).unwrap())
            .collect();

        // then
        let raw_data: Vec<u8> = blobs.iter().flat_map(|b| b.as_slice()).copied().collect();
        let bits = BitVec::<u8, Msb0>::from_vec(raw_data);

        let num_zeroes_at_end = bits.iter().rev().take_while(|b| *b == false).count() as u64;

        let max_num_bits = alloy::eips::eip4844::FIELD_ELEMENTS_PER_BLOB
            * alloy::eips::eip4844::FIELD_ELEMENT_BYTES
            * 8;

        for (expected_idx, header) in headers.iter().enumerate() {
            let expected_last = expected_idx == headers.len() - 1;

            let num_bits = if expected_last {
                max_num_bits - num_zeroes_at_end
            } else {
                max_num_bits
            };

            assert_eq!(
                header,
                &blob::Header::V1(blob::HeaderV1 {
                    bundle_id,
                    num_bits: num_bits as u32,
                    is_last: expected_last,
                    idx: expected_idx as u32
                })
            );
        }
    }
}
