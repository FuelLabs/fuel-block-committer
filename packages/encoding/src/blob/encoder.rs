use std::marker::PhantomData;

use bitvec::{order::Msb0, slice::BitSlice};
use static_assertions::const_assert;
mod storage;
use storage::Storage;

use crate::constants::{FIELD_ELEMENTS_PER_BLOB, USABLE_BITS_PER_FIELD_ELEMENT};

use super::{header::Header, Blob};

#[derive(Default, Debug, Clone)]
pub struct Encoder {
    _private: PhantomData<()>,
}

impl Encoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Encoder {
    #[must_use]
    pub const fn blobs_needed_to_encode(&self, num_bytes: usize) -> usize {
        #[allow(clippy::cast_possible_truncation)]
        const USABLE_BITS_PER_BLOB: usize = USABLE_BITS_PER_FIELD_ELEMENT * FIELD_ELEMENTS_PER_BLOB;

        const NUM_BITS_FOR_METADATA: usize = Header::V1_SIZE_BITS;

        const NUM_BYTES_FOR_DATA: usize =
            (USABLE_BITS_PER_BLOB - NUM_BITS_FOR_METADATA).saturating_div(8);

        const_assert!(NUM_BYTES_FOR_DATA > 0);

        num_bytes.div_ceil(NUM_BYTES_FOR_DATA)
    }

    pub fn encode(&self, orig_data: &[u8], id: u32) -> anyhow::Result<Vec<Blob>> {
        let mut storage = Storage::new();

        let mut data = BitSlice::<u8, Msb0>::from_slice(orig_data);
        while !data.is_empty() {
            let amount_ingested = storage.ingest(data);
            data = &data[amount_ingested..];
        }

        Ok(storage.finalize(id))
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
