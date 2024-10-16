use std::{cmp::min, marker::PhantomData};

use alloy::eips::eip4844::{
    BYTES_PER_BLOB, FIELD_ELEMENTS_PER_BLOB, USABLE_BITS_PER_FIELD_ELEMENT,
};
use anyhow::Result;
use bitvec::{array::BitArray, order::Msb0, slice::BitSlice};

use super::{header::Header, Blob};
use crate::blob::header::HeaderV1;

#[derive(Default, Debug, Clone)]
pub struct Encoder {
    _private: PhantomData<()>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }
}

const BITS_PER_FE: usize = 256;
const BITS_PER_BLOB: usize = FIELD_ELEMENTS_PER_BLOB as usize * BITS_PER_FE;

impl Encoder {
    const USABLE_BITS_PER_BLOB: usize =
        USABLE_BITS_PER_FIELD_ELEMENT * FIELD_ELEMENTS_PER_BLOB as usize;

    const NUM_BITS_FOR_METADATA: usize = Header::V1_SIZE_BITS;
    const NUM_BITS_FOR_DATA: usize = Self::USABLE_BITS_PER_BLOB - Self::NUM_BITS_FOR_METADATA;

    pub fn blobs_needed_to_encode(&self, num_bytes: usize) -> usize {
        num_bytes.div_ceil(Self::NUM_BITS_FOR_DATA.saturating_div(8))
    }

    pub fn encode(&self, orig_data: &[u8], id: u32) -> Result<Vec<Blob>> {
        let mut storage = BlobStorage::new();

        let mut data_to_consume = BitSlice::<u8, Msb0>::from_slice(orig_data);
        while !data_to_consume.is_empty() {
            if storage.at_start_of_new_blob() {
                storage.allocate();
                storage.skip_two_bits();
                storage.skip_header_bits();
            } else if storage.at_start_of_new_fe() {
                storage.skip_two_bits();
            }

            let available_fe_space = storage.bits_until_fe_end();

            let data_len = min(available_fe_space, data_to_consume.len());
            let data = &data_to_consume[..data_len];

            storage.ingest(data);
            data_to_consume = &data_to_consume[data_len..];
        }

        Ok(storage.finalize(id))
    }
}

struct BlobStorage {
    blobs: Vec<BitArray<[u8; BYTES_PER_BLOB], Msb0>>,
    bit_counter: usize,
}

impl BlobStorage {
    fn new() -> Self {
        Self {
            blobs: vec![],
            bit_counter: 0,
        }
    }

    fn bits_until_fe_end(&self) -> usize {
        BITS_PER_FE - self.bit_counter % BITS_PER_FE
    }

    fn at_start_of_new_fe(&self) -> bool {
        self.bit_counter % BITS_PER_FE == 0
    }

    fn at_start_of_new_blob(&self) -> bool {
        self.bit_counter % BITS_PER_BLOB == 0
    }

    fn current_blob_idx(&self) -> usize {
        self.blobs.len().saturating_sub(1)
    }

    fn allocate(&mut self) {
        self.blobs.push(BitArray::ZERO);
    }

    fn skip_two_bits(&mut self) {
        self.bit_counter += 2;
    }

    fn skip_header_bits(&mut self) {
        const {
            if Header::V1_SIZE_BITS > USABLE_BITS_PER_FIELD_ELEMENT {
                panic!("The current implementation of the encoder requires the blob header to be <= 254 bits")
            }
        }

        self.bit_counter += Header::V1_SIZE_BITS;
    }

    fn ingest(&mut self, bits: &BitSlice<u8, Msb0>) {
        debug_assert!(self.bits_until_fe_end() >= bits.len());

        let blob_idx = self.current_blob_idx();
        let start_free_blob_space = self.bit_counter % BITS_PER_BLOB;

        let dst = &mut self.blobs[blob_idx][start_free_blob_space..];
        dst[..bits.len()].copy_from_bitslice(bits);

        self.bit_counter += bits.len();
    }

    fn finalize(self, id: u32) -> Vec<Blob> {
        let idx_of_last_blob = self.current_blob_idx();
        self.blobs
            .into_iter()
            .enumerate()
            .map(|(idx, mut blob)| {
                let is_last = idx == idx_of_last_blob;

                let remainder = self.bit_counter % BITS_PER_BLOB;
                let num_bits = if !is_last || remainder == 0 {
                    BITS_PER_BLOB
                } else {
                    remainder
                };
                eprintln!("num_bits = {:?}", num_bits);

                let header = Header::V1(HeaderV1 {
                    bundle_id: id,
                    num_bits: num_bits as u32,
                    is_last,
                    idx: idx as u32,
                });

                // Checked during compile time that the BlobHeader, when encoded, won't be
                // bigger than 254 bits so we don't have to worry about writing over the first 2
                // bits of a FE.
                blob[2..][..Header::V1_SIZE_BITS].copy_from_bitslice(&header.encode());

                Box::new(blob.into_inner())
            })
            .collect()
    }
}
