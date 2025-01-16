use std::marker::PhantomData;

use bitvec::{order::Msb0, slice::BitSlice, vec::BitVec};

use super::{header::Header, Blob};
mod validator;

#[derive(Default, Debug, Clone)]
pub struct Decoder {
    _private: PhantomData<()>,
}

impl Decoder {
    pub fn decode(&self, blobs: &[Blob]) -> anyhow::Result<Vec<u8>> {
        let blobs = validator::Validator::for_blobs(blobs)?;

        let data = {
            let mut data_bits = BitVec::<u8, Msb0>::new();

            for blob in blobs.validated_blobs()? {
                let mut field_element_data = blob.chunks(256).map(|chunk| &chunk[2..]);

                let first_chunk = field_element_data.next();

                if let Some(chunk) = first_chunk {
                    data_bits.extend_from_bitslice(&chunk[Header::V1_SIZE_BITS..]);
                }

                for chunk in field_element_data {
                    data_bits.extend_from_bitslice(chunk);
                }
            }

            data_bits.into_vec()
        };

        Ok(data)
    }

    pub fn read_header(&self, blob: &Blob) -> anyhow::Result<Header> {
        let buffer = BitSlice::<u8, Msb0>::from_slice(blob.as_slice());

        let buffer = &buffer[2..];
        let (header, _) = Header::decode(buffer)?;

        Ok(header)
    }
}

#[cfg(test)]
mod tests {
    use bitvec::{order::Msb0, vec::BitVec};

    use crate::blob::{self, Blob, Header};

    #[test]
    fn complains_if_no_blobs_are_given() {
        // given
        let decoder = super::Decoder::default();

        // when
        let err = decoder.decode(&[]).unwrap_err();

        // then
        assert_eq!(err.to_string(), "No blobs to decode");
    }

    #[test]
    fn complains_if_there_is_a_blob_from_a_different_bundle() {
        // given
        let bundle_1_blobs = blob::Encoder::default().encode(&[100], 0).unwrap();
        let bundle_2_blobs = blob::Encoder::default().encode(&[100], 1).unwrap();

        let all_blobs = [bundle_1_blobs, bundle_2_blobs].concat();

        // when
        let err = super::Decoder::default().decode(&all_blobs).unwrap_err();

        // then
        assert_eq!(
            err.to_string(),
            "All blobs must have the same bundle id, got {0, 1}"
        );
    }

    #[test]
    fn complains_if_there_are_duplicate_idx() {
        // given
        let bundle_1_blobs = blob::Encoder::default().encode(&[0; 200_000], 0).unwrap();
        assert_eq!(bundle_1_blobs.len(), 2);

        let with_duplication = [
            bundle_1_blobs[0].clone(),
            bundle_1_blobs[0].clone(),
            bundle_1_blobs[1].clone(),
        ];

        // when
        let err = super::Decoder::default()
            .decode(&with_duplication)
            .unwrap_err();

        // then
        assert_eq!(err.to_string(), "found duplicate blob idxs: 0");
    }

    #[test]
    fn complains_if_index_missing() {
        // given
        let blobs = blob::Encoder::default()
            .encode(&vec![0; 400_000], 0)
            .unwrap();
        assert_eq!(blobs.len(), 4);

        let blobs_with_holes = [blobs[0].clone(), blobs[3].clone()];

        // when
        let err = super::Decoder::default()
            .decode(&blobs_with_holes)
            .unwrap_err();

        // then
        assert_eq!(err.to_string(), "missing blobs with indexes: 1, 2");
    }

    #[test]
    fn complains_if_no_blob_is_marked_as_last() {
        // given
        let blobs = blob::Encoder::default()
            .encode(&vec![0; 400_000], 0)
            .unwrap();
        assert_eq!(blobs.len(), 4);

        let leave_out_the_last_blob = &blobs[..3];

        // when
        let err = super::Decoder::default()
            .decode(leave_out_the_last_blob)
            .unwrap_err();

        // then
        assert_eq!(err.to_string(), "no blob is marked as last");
    }

    #[test]
    fn multiple_blobs_marked_as_being_the_last() {
        // given
        let four_blob_bundle = blob::Encoder::default()
            .encode(&vec![0; 400_000], 0)
            .unwrap();
        assert_eq!(four_blob_bundle.len(), 4);

        let three_blob_bundle = blob::Encoder::default()
            .encode(&vec![0; 300_000], 0)
            .unwrap();
        assert_eq!(three_blob_bundle.len(), 3);

        let blobs_with_multiple_last = [
            four_blob_bundle[0].clone(),
            four_blob_bundle[1].clone(),
            three_blob_bundle[2].clone(),
            four_blob_bundle[3].clone(),
        ];

        // when
        let err = super::Decoder::default()
            .decode(&blobs_with_multiple_last)
            .unwrap_err();

        // then
        assert_eq!(
            err.to_string(),
            "multiple blobs marked as being the last blob. blobs with indexes: 2, 3"
        );
    }

    #[test]
    fn complains_if_the_last_blob_doesnt_have_the_highest_idx() {
        // given
        let four_blob_bundle = blob::Encoder::default()
            .encode(&vec![0; 400_000], 0)
            .unwrap();
        assert_eq!(four_blob_bundle.len(), 4);

        let two_blob_bundle = blob::Encoder::default()
            .encode(&vec![0; 200_000], 0)
            .unwrap();
        assert_eq!(two_blob_bundle.len(), 2);

        let blobs_with_last_not_highest_idx = [
            four_blob_bundle[0].clone(),
            four_blob_bundle[1].clone(),
            four_blob_bundle[2].clone(),
            two_blob_bundle[1].clone(),
        ];

        // when
        let err = super::Decoder::default()
            .decode(&blobs_with_last_not_highest_idx)
            .unwrap_err();

        // then
        assert_eq!(
            err.to_string(),
            "blob with highest index is 2, but the blob marked as last has index 1"
        );
    }

    #[test]
    fn fails_if_num_of_bits_more_than_what_can_fit_in_a_blob() {
        // given
        let mut blobs = blob::Encoder::default()
            .encode(&vec![0; 100_000], 0)
            .unwrap();
        assert_eq!(blobs.len(), 1);
        let blob = blobs.pop().unwrap();
        let mut blob_data = BitVec::<u8, Msb0>::from_slice(blob.as_slice());

        let corrupted_header = blob::Header::V1(blob::HeaderV1 {
            bundle_id: 0,
            num_bits: 256 * 4096 + 1,
            is_last: true,
            idx: 0,
        });

        blob_data[2..2 + Header::V1_SIZE_BITS].copy_from_bitslice(&corrupted_header.encode());
        let corrupted_blob: Blob = blob_data.into_vec().into_boxed_slice().try_into().unwrap();

        // when
        let err = super::Decoder::default()
            .decode(&[corrupted_blob])
            .unwrap_err();

        // then
        assert_eq!(
            err.to_string(),
            "num_bits of blob (bundle_id: 0, idx: 0) is greater than the maximum allowed value of 1048576"
        );
    }
}
