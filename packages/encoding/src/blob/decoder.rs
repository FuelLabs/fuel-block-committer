use std::marker::PhantomData;

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

        // Ensure that all indices are consecutive and starting from zero
        for (expected_idx, (header, _)) in indexed_data.iter().enumerate() {
            if expected_idx as u32 != header.idx {
                bail!("Expected index {}, got {}", expected_idx, header.idx);
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
