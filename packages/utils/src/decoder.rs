use anyhow::bail;
use bitvec::{
    order::{Lsb0, Msb0},
    slice::BitSlice,
    vec::BitVec,
};

use crate::{Blob, BlobHeader, BlobHeaderV1, BlobWithProof};

pub struct NewDecoder {}
impl NewDecoder {
    pub fn decode(&self, blobs: &[Blob]) -> anyhow::Result<Vec<u8>> {
        let mut indexed_data = blobs
            .iter()
            .map(|blob| {
                // Convert the blob into a BitSlice for bit-level manipulation
                let buffer = BitSlice::<u8, Msb0>::from_slice(blob.as_slice());

                // Decode the header from the buffer (starting from index 2)
                let (header, read_bits) = BlobHeader::decode(&buffer[2..])?;
                let BlobHeader::V1(header) = header;
                eprintln!("{:?}", header);

                // Calculate the start and end indices for the data
                let data_start = read_bits + 2; // +2 to adjust for the initial slicing
                let data_end = header.num_bits as usize;

                // Slice the data excluding the header bits
                let data = &buffer[..data_end];

                // Return the index and the data slice
                Ok((header.idx, data, data_start))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Sort the data by their indices to maintain order
        indexed_data.sort_by_key(|(idx, _, _)| *idx);

        // Ensure that all indices are consecutive and starting from zero
        for (expected_idx, (actual_idx, _, _)) in indexed_data.iter().enumerate() {
            if expected_idx as u32 != *actual_idx {
                bail!("Expected index {}, got {}", expected_idx, actual_idx);
            }
        }

        // Collect all data, skipping the first two bits of every 256-bit chunk
        let data = {
            let mut data_bits = BitVec::<u8, Msb0>::new();

            for (_, data_slice, initial_offset) in indexed_data {
                eprintln!("initial_offset = {:?}", initial_offset);

                let mut offset = 0;
                let data_len = data_slice.len() + initial_offset;

                let mut skip = initial_offset;
                while offset < data_len {
                    // Determine the size of the next chunk
                    let chunk_size = 256 - skip;
                    eprintln!("chunk_size = {:?}", chunk_size);

                    // Get the chunk from the data slice
                    eprintln!("offset = {:?}", offset);
                    eprintln!("skip = {:?}", skip);

                    eprintln!(
                        "data_slice[{} + {}..{} + {}] = data_slice[{}..{}]",
                        offset,
                        skip,
                        offset,
                        chunk_size,
                        offset + skip,
                        offset + chunk_size
                    );

                    let chunk = &data_slice[offset + skip..offset + chunk_size];

                    // Extend our BitVec with the processed chunk
                    data_bits.extend_from_bitslice(chunk);

                    // Move the offset forward
                    offset += chunk_size;

                    skip = 0;
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
