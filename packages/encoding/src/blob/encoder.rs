use std::marker::PhantomData;

use alloy::eips::eip4844::{FIELD_ELEMENTS_PER_BLOB, USABLE_BITS_PER_FIELD_ELEMENT};
use bitvec::{order::Msb0, slice::BitSlice};
use static_assertions::const_assert;
use storage::BlobStorage;

use super::{header::Header, Blob};

#[derive(Default, Debug, Clone)]
pub struct Encoder {
    _private: PhantomData<()>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Encoder {
    pub fn blobs_needed_to_encode(&self, num_bytes: usize) -> usize {
        const USABLE_BITS_PER_BLOB: usize =
            USABLE_BITS_PER_FIELD_ELEMENT * FIELD_ELEMENTS_PER_BLOB as usize;

        const NUM_BITS_FOR_METADATA: usize = Header::V1_SIZE_BITS;

        const NUM_BYTES_FOR_DATA: usize =
            (USABLE_BITS_PER_BLOB - NUM_BITS_FOR_METADATA).saturating_div(8);

        const_assert!(NUM_BYTES_FOR_DATA > 0);

        num_bytes.div_ceil(NUM_BYTES_FOR_DATA)
    }

    pub fn encode(&self, orig_data: &[u8], id: u32) -> anyhow::Result<Vec<Blob>> {
        let mut storage = BlobStorage::new();

        let mut data = BitSlice::<u8, Msb0>::from_slice(orig_data);
        while !data.is_empty() {
            let amount_ingested = storage.ingest(data);
            data = &data[amount_ingested..];
        }

        Ok(storage.finalize(id))
    }
}

mod storage {
    use std::cmp::min;

    use bitvec::{array::BitArray, order::Msb0, slice::BitSlice};
    pub struct BlobStorage {
        blobs: Vec<BitArray<[u8; BYTES_PER_BLOB], Msb0>>,
        bit_counter: usize,
    }
    use alloy::eips::eip4844::{
        BYTES_PER_BLOB, FIELD_ELEMENTS_PER_BLOB, USABLE_BITS_PER_FIELD_ELEMENT,
    };
    use static_assertions::const_assert;

    use crate::blob::{Blob, Header, HeaderV1};

    const BITS_PER_FE: usize = 256;
    const BITS_PER_BLOB: usize = FIELD_ELEMENTS_PER_BLOB as usize * BITS_PER_FE;

    impl BlobStorage {
        pub fn new() -> Self {
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
            debug_assert!(self.bits_until_fe_end() >= 2);
            self.advance_bit_counter(2);
        }

        fn skip_header_bits(&mut self) {
            debug_assert!(self.bits_until_fe_end() >= Header::V1_SIZE_BITS);
            // The current implementation of the encoder requires the blob header to be <= 254 bits
            const_assert!(Header::V1_SIZE_BITS <= USABLE_BITS_PER_FIELD_ELEMENT);

            self.advance_bit_counter(Header::V1_SIZE_BITS);
        }

        fn advance_bit_counter(&mut self, num_bits: usize) {
            self.bit_counter = self
                .bit_counter
                .checked_add(num_bits)
                .expect("never to encode more more than usize::MAX / 8 bytes");
        }

        pub fn ingest(&mut self, data: &BitSlice<u8, Msb0>) -> usize {
            if data.is_empty() {
                return 0;
            }

            if self.at_start_of_new_blob() {
                self.allocate();
                self.skip_two_bits();
                self.skip_header_bits();
            } else if self.at_start_of_new_fe() {
                self.skip_two_bits();
            }

            let available_fe_space = self.bits_until_fe_end();

            let data_len = min(available_fe_space, data.len());

            let data_to_ingest = &data[..data_len];

            debug_assert!(self.bits_until_fe_end() >= data_len);

            let blob_idx = self.current_blob_idx();
            let start_free_blob_space = self.bit_counter % BITS_PER_BLOB;

            let dst = &mut self.blobs[blob_idx][start_free_blob_space..];
            dst[..data_to_ingest.len()].copy_from_bitslice(data_to_ingest);

            self.advance_bit_counter(data_to_ingest.len());

            data_len
        }

        pub fn finalize(self, id: u32) -> Vec<Blob> {
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

    #[cfg(test)]
    mod tests {

        use super::*;

        #[test]
        fn ingesting_empty_data_doesnt_allocate() {
            // given
            let mut storage = BlobStorage::new();

            // when
            storage.ingest(BitSlice::empty());

            // then
            assert!(storage.finalize(0).is_empty());
        }

        #[test]
        fn consuming_exactly_one_blob_doesnt_allocate_another() {
            // given
            let mut storage = BlobStorage::new();

            // when
            let data = bitvec::bitvec![u8, Msb0; 0; 4096 * 254 - Header::V1_SIZE_BITS];
            let mut data_slice = &data[..];
            while !data_slice.is_empty() {
                let ingested = storage.ingest(data_slice);
                data_slice = &data_slice[ingested..];
            }
            assert!(storage.at_start_of_new_blob());

            // then
            assert_eq!(storage.finalize(0).len(), 1);
        }
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn can_handle_zero_input() {
        // given
        let no_data = [];

        // when
        let blobs = super::Encoder::new().encode(&no_data, 0).unwrap();

        // then
        assert!(blobs.is_empty());
    }
}
