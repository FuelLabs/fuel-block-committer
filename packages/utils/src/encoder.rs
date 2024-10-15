use alloy::eips::eip4844::{BYTES_PER_BLOB, USABLE_BYTES_PER_BLOB};
use anyhow::Result;
use bitvec::{array::BitArray, bitarr, bitvec, field::BitField, order::Lsb0, vec::BitVec, BitArr};

use crate::{Blob, BlobHeader, BlobHeaderV1, BlobWithProof};

pub struct NewEncoder {}

impl NewEncoder {
    pub fn blobs_needed_to_encode(&self, num_bytes: usize) -> usize {
        const USABLE_BITS_PER_BLOB: usize = USABLE_BYTES_PER_BLOB.saturating_mul(8);

        let num_bits = num_bytes.saturating_mul(8);

        let with_header = num_bits.saturating_add(BlobHeaderV1::SIZE);

        with_header.div_ceil(USABLE_BITS_PER_BLOB)
    }

    pub fn encode(&self, data: &[u8], id: u32) -> Result<Vec<Blob>> {
        let header = BlobHeader::V1(BlobHeaderV1 {
            size: data.len() as u32 + BlobHeaderV1::SIZE as u32,
            bundle_id: id,
            is_last: true,
            idx: 0,
        });

        let mut data = BitVec::<u8, Lsb0>::new();

        header.encode(&mut data);

        let final_data = data.into_vec();

        let mut blob = Box::new([0; BYTES_PER_BLOB]);

        blob[1..][..final_data.len()].copy_from_slice(&final_data);

        Ok(vec![blob])
    }
}
