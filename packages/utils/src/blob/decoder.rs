use std::marker::PhantomData;

use anyhow::bail;
use bitvec::{order::Msb0, slice::BitSlice, vec::BitVec};

use super::{header::BlobHeader, Blob};

#[derive(Default, Debug, Clone)]
pub struct NewDecoder {
    _private: PhantomData<()>,
}

impl NewDecoder {
    pub fn decode(&self, blobs: &[Blob]) -> anyhow::Result<Vec<u8>> {
        let mut indexed_data = blobs
            .iter()
            .map(|blob| {
                // Convert the blob into a BitSlice for bit-level manipulation
                let buffer = BitSlice::<u8, Msb0>::from_slice(blob.as_slice());

                // Decode the header from the buffer (starting from index 2)
                let (header, _) = BlobHeader::decode(&buffer[2..])?;
                let BlobHeader::V1(header) = header;

                // Calculate the start and end indices for the data
                let data_end = header.num_bits as usize;

                // Slice the data excluding the header bits
                let data = &buffer[..data_end];

                // Return the index and the data slice
                Ok((header.idx, data))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Sort the data by their indices to maintain order
        indexed_data.sort_by_key(|(idx, _)| *idx);

        // Ensure that all indices are consecutive and starting from zero
        for (expected_idx, (actual_idx, _)) in indexed_data.iter().enumerate() {
            if expected_idx as u32 != *actual_idx {
                bail!("Expected index {}, got {}", expected_idx, actual_idx);
            }
        }

        // Collect all data, skipping the first two bits of every 256-bit chunk
        let data = {
            let mut data_bits = BitVec::<u8, Msb0>::new();

            for (_, data_slice) in indexed_data {
                let mut chunks = data_slice.chunks(256);

                let first_chunk = chunks.next();

                if let Some(chunk) = first_chunk {
                    let (_, read) = BlobHeader::decode(&chunk[2..])?;
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

    pub fn read_header(&self, blob: &Blob) -> anyhow::Result<BlobHeader> {
        let buffer = BitSlice::<u8, Msb0>::from_slice(blob.as_slice());

        let buffer = &buffer[2..];
        let (header, _) = BlobHeader::decode(buffer)?;

        Ok(header)
    }
}
