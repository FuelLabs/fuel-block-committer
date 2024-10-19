use std::{collections::HashSet, marker::PhantomData};

use anyhow::bail;
use bitvec::{order::Msb0, slice::BitSlice, vec::BitVec};

use super::{header::Header, Blob};

#[derive(Default, Debug, Clone)]
pub struct Decoder {
    _private: PhantomData<()>,
}

impl Decoder {
    pub fn decode(&self, blobs: &[Blob]) -> anyhow::Result<Vec<u8>> {
        if blobs.is_empty() {
            bail!("No blobs to decode");
        }

        // TODO: check the num bits being out of bounds of a blob size

        let mut indexed_data = blobs
            .iter()
            .map(|blob| {
                // Convert the blob into a BitSlice for bit-level manipulation
                let buffer = BitSlice::<u8, Msb0>::from_slice(blob.as_slice());

                // Decode the header from the buffer (starting from index 2)
                let (header, _) = Header::decode(&buffer[2..])?;
                let Header::V1(header) = header;

                // Calculate the start and end indices for the data
                let data_end = header.num_bits as usize;

                // Slice the data excluding the header bits
                let data = &buffer[..data_end];

                // Return the index and the data slice
                Ok((header, data))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Sort the data by their indices to maintain order
        indexed_data.sort_by_key(|(header, _)| header.idx);

        // TODO: segfault add check for when a blob has a higher idx than the ending blob

        // make sure all blobs have the same bundle id
        indexed_data.iter().skip(1).try_for_each(|(header, _)| {
            if header.bundle_id != indexed_data[0].0.bundle_id {
                bail!(
                    "All blobs must have the same bundle id, got {} and {}",
                    header.bundle_id,
                    indexed_data[0].0.bundle_id
                );
            }
            Ok(())
        })?;

        let blobs_marked_as_last: Vec<_> = indexed_data
            .iter()
            .filter(|(header, _)| header.is_last)
            .collect();

        if blobs_marked_as_last.is_empty() {
            bail!("no blob is marked as last");
        }

        let highest_idx = indexed_data.last().expect("At least one blob").0.idx;
        if blobs_marked_as_last.len() > 1 {
            let msg = blobs_marked_as_last
                .iter()
                .map(|(header, _)| header.idx.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("multiple blobs marked as being the last blob. blobs with indexes: {msg}");
        }

        if blobs_marked_as_last[0].0.idx != highest_idx {
            bail!(
                "blob with highest index is {}, but the blob marked as last has index {}",
                highest_idx,
                blobs_marked_as_last[0].0.idx
            );
        }

        let present_idxs: HashSet<u32> =
            indexed_data.iter().map(|(header, _)| header.idx).collect();

        let missing_idxs: Vec<u32> = (0..=highest_idx)
            .filter(|idx| !present_idxs.contains(idx))
            .collect();

        if !missing_idxs.is_empty() {
            let msg = missing_idxs
                .iter()
                .map(|idx| idx.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("missing blobs with indexes: {msg}");
        }

        // Ensure that all indices are consecutive and starting from zero
        for (expected_idx, (header, _)) in indexed_data.iter().enumerate() {
            if expected_idx as u32 != header.idx {
                bail!(
                    "unexpected blob idx of {}, expected the idx of {}",
                    header.idx,
                    expected_idx
                );
            }
        }

        // Collect all data, skipping the first two bits of every 256-bit chunk
        let data = {
            let mut data_bits = BitVec::<u8, Msb0>::new();

            for (_, data_slice) in indexed_data {
                let mut chunks = data_slice.chunks(256);

                let first_chunk = chunks.next();

                if let Some(chunk) = first_chunk {
                    let (_, read) = Header::decode(&chunk[2..])?;
                    data_bits.extend_from_bitslice(&chunk[read + 2..]);
                }

                for chunk in chunks {
                    data_bits.extend_from_bitslice(&chunk[2..]);
                }
            }

            // Convert the BitVec into a Vec<u8>
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
mod test {
    use crate::blob;

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
            "All blobs must have the same bundle id, got 1 and 0"
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
        assert_eq!(
            err.to_string(),
            "unexpected blob idx of 0, expected the idx of 1"
        );
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
}
