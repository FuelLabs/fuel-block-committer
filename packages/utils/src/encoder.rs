use std::cmp::min;

use alloy::eips::eip4844::{
    BYTES_PER_BLOB, FIELD_ELEMENTS_PER_BLOB, USABLE_BITS_PER_FIELD_ELEMENT,
};
use anyhow::Result;
use bitvec::{array::BitArray, order::Msb0, slice::BitSlice};

use crate::{Blob, BlobHeader, BlobHeaderV1};

pub struct NewEncoder {}

const BITS_PER_FE: usize = 256;
const BITS_PER_BLOB: usize = FIELD_ELEMENTS_PER_BLOB as usize * BITS_PER_FE;

struct Inner {
    blobs: Vec<BitArray<[u8; BYTES_PER_BLOB], Msb0>>,
    bit_counter: usize,
}

impl Inner {
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
        // eprintln!("skip_two_bits");
        let current = self.bit_counter;
        self.bit_counter += 2;
        // eprintln!(
        //     "old = {:?}, self.bit_counter = {:?}",
        //     current, self.bit_counter
        // );
    }

    fn skip_header_bits(&mut self) {
        const {
            if BlobHeader::V1_SIZE_BITS > USABLE_BITS_PER_FIELD_ELEMENT {
                panic!("The current implementation of the encoder requires the blob header to be <= 254 bits")
            }
        }

        let current = self.bit_counter;
        self.bit_counter += BlobHeader::V1_SIZE_BITS;
        // eprintln!(
        //     "old = {:?}, self.bit_counter = {:?}",
        //     current, self.bit_counter
        // );
    }

    fn ingest(&mut self, bits: &BitSlice<u8, Msb0>) {
        debug_assert!(self.bits_until_fe_end() >= bits.len());

        let blob_idx = self.current_blob_idx();
        let start_free_blob_space = self.bit_counter % BITS_PER_BLOB;

        let dst = &mut self.blobs[blob_idx][start_free_blob_space..];
        dst[..bits.len()].copy_from_bitslice(bits);
        // eprintln!(
        //     "blobs[{}][{}..][..{}]",
        //     blob_idx,
        //     start_free_blob_space,
        //     bits.len()
        // );

        let current = self.bit_counter;

        self.bit_counter += bits.len();

        // eprintln!(
        //     "old = {:?}, self.bit_counter = {:?}",
        //     current, self.bit_counter
        // );
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

                let header = BlobHeader::V1(BlobHeaderV1 {
                    bundle_id: id,
                    num_bits: num_bits as u32,
                    is_last,
                    idx: idx as u32,
                });

                // Checked during compile time that the BlobHeader, when encoded, won't be
                // bigger than 254 bits so we don't have to worry about writing over the first 2
                // bits of a FE.
                blob[2..][..BlobHeader::V1_SIZE_BITS].copy_from_bitslice(&header.encode());

                // for slice in blob.chunks(256) {
                //     let bytes: Vec<_> = slice.bytes().map(|b| b.unwrap()).collect();
                //     let arr: [u8; const { 256 / 8 }] = bytes.try_into().unwrap();
                //     let num = U256::from_be_bytes(arr);
                //
                //     // eprintln!("{num}");
                //     assert!(num < BLS_MODULUS);
                //     eprintln!("{}", &slice[..2]);
                // }

                let f = Box::new(blob.into_inner());
                // eprintln!("{}", alloy::hex::encode(f.as_slice()));
                f
            })
            .collect()
    }
}

impl NewEncoder {
    const USABLE_BITS_PER_BLOB: usize =
        USABLE_BITS_PER_FIELD_ELEMENT * FIELD_ELEMENTS_PER_BLOB as usize;

    const NUM_BITS_FOR_METADATA: usize = BlobHeader::V1_SIZE_BITS;
    const NUM_BITS_FOR_DATA: usize = Self::USABLE_BITS_PER_BLOB - Self::NUM_BITS_FOR_METADATA;

    pub fn blobs_needed_to_encode(&self, num_bytes: usize) -> usize {
        num_bytes.div_ceil(Self::NUM_BITS_FOR_DATA.saturating_div(8))
    }

    pub fn encode(&self, orig_data: &[u8], id: u32) -> Result<Vec<Blob>> {
        let mut data_to_consume = BitSlice::<u8, Msb0>::from_slice(orig_data);
        let mut inner = Inner::new();

        while !data_to_consume.is_empty() {
            // eprintln!("data_to_consume.len() = {:?}", data_to_consume.len());
            if inner.at_start_of_new_blob() {
                // eprintln!("at_start_of_new_blob");
                inner.allocate();
                inner.skip_two_bits();
                inner.skip_header_bits();
            } else if inner.at_start_of_new_fe() {
                // eprintln!("at_start_of_new_fe");
                inner.skip_two_bits();
            }

            let available_fe_space = inner.bits_until_fe_end();
            // eprintln!("available_fe_space = {:?}", available_fe_space);

            let data_len = min(available_fe_space, data_to_consume.len());
            let data = &data_to_consume[..data_len];

            // eprintln!("ingesting data.len() = {:?}", data.len());
            inner.ingest(data);
            data_to_consume = &data_to_consume[data_len..];
        }

        Ok(inner.finalize(id))
    }
}
