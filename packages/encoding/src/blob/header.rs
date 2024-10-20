use anyhow::{bail, Result};
use bitvec::{field::BitField, order::Msb0, slice::BitSlice, vec::BitVec, view::BitView};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HeaderV1 {
    pub bundle_id: u32,
    /// Number of bits containing data
    pub num_bits: u32,
    pub is_last: bool,
    pub idx: u32,
}

impl HeaderV1 {
    const BUNDLE_ID_BITS: usize = 32;
    const NUM_BITS_BITS: usize = 21;
    const IS_LAST_BITS: usize = 1;
    const IDX_SIZE_BITS: usize = 17;
    pub const TOTAL_SIZE_BITS: usize =
        Self::BUNDLE_ID_BITS + Self::NUM_BITS_BITS + Self::IS_LAST_BITS + Self::IDX_SIZE_BITS;

    fn encode(&self, buffer: &mut BitVec<u8, Msb0>) {
        buffer.extend_from_bitslice(self.bundle_id.view_bits::<Msb0>());

        buffer.extend_from_bitslice(&self.num_bits.view_bits::<Msb0>()[32 - Self::NUM_BITS_BITS..]);

        buffer.push(self.is_last);

        buffer.extend_from_bitslice(&self.idx.view_bits::<Msb0>()[32 - Self::IDX_SIZE_BITS..]);
    }

    fn decode(data: &BitSlice<u8, Msb0>) -> Result<(Self, usize)> {
        if data.len() < Self::TOTAL_SIZE_BITS {
            bail!(
                "not enough data to decode header, expected {} bits, got {}",
                Self::TOTAL_SIZE_BITS,
                data.len()
            )
        }

        let bundle_id = data[..Self::BUNDLE_ID_BITS].load_be();
        let remaining_data = &data[Self::BUNDLE_ID_BITS..];

        let num_bits = remaining_data[..Self::NUM_BITS_BITS].load_be::<u32>();
        let remaining_data = &remaining_data[Self::NUM_BITS_BITS..];

        let is_last = remaining_data[0];
        let remaining_data = &remaining_data[1..];

        let idx = remaining_data[..Self::IDX_SIZE_BITS].load_be::<u32>();
        let remaining_data = &remaining_data[Self::IDX_SIZE_BITS..];

        let header = Self {
            bundle_id,
            num_bits,
            is_last,
            idx,
        };

        let amount_read = data.len().saturating_sub(remaining_data.len());

        Ok((header, amount_read))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Header {
    V1(HeaderV1),
}

impl Header {
    const VERSION_BITS: usize = 16;
    pub const V1_SIZE_BITS: usize = HeaderV1::TOTAL_SIZE_BITS + Self::VERSION_BITS;

    pub(crate) fn encode(&self) -> BitVec<u8, Msb0> {
        match self {
            Self::V1(blob_header_v1) => {
                let mut buffer = BitVec::<u8, Msb0>::new();

                let version = 1u16;
                buffer.extend_from_bitslice(version.view_bits::<Msb0>());

                blob_header_v1.encode(&mut buffer);

                buffer
            }
        }
    }
    pub(crate) fn decode(data: &BitSlice<u8, Msb0>) -> Result<(Self, usize)> {
        if data.len() < Self::VERSION_BITS {
            bail!(
                "not enough data to decode header version, expected {} bits, got {}",
                Self::VERSION_BITS,
                data.len()
            );
        }

        let version = data[..Self::VERSION_BITS].load_be::<u16>();

        let remaining_data = &data[Self::VERSION_BITS..];
        let read_version_bits = data
            .len()
            .checked_sub(remaining_data.len())
            .expect("remaining_data should always be smaller than data");
        match version {
            1 => {
                let (header, read_bits) = HeaderV1::decode(remaining_data)?;
                Ok((
                    Self::V1(header),
                    read_bits
                        .checked_add(read_version_bits)
                        .expect("never to encode more than usize bits"),
                ))
            }
            version => bail!("Unsupported version {version}"),
        }
    }
}

#[cfg(test)]
mod test {
    use bitvec::{field::BitField, order::Msb0};

    use super::Header;

    #[test]
    fn detects_unsupported_version() {
        // given
        let mut encoded_header = Header::V1(super::HeaderV1 {
            bundle_id: 0,
            num_bits: 0,
            is_last: false,
            idx: 0,
        })
        .encode();

        encoded_header[0..16].store_be(2u16);

        // when
        let header = Header::decode(&encoded_header).unwrap_err();

        // then
        assert_eq!(header.to_string(), "Unsupported version 2");
    }

    #[test]
    fn complains_if_not_enough_data_is_given_for_version() {
        // given
        let encoded_header = Header::V1(super::HeaderV1 {
            bundle_id: 0,
            num_bits: 0,
            is_last: false,
            idx: 0,
        })
        .encode();

        // when
        let err = Header::decode(&encoded_header[..10]).unwrap_err();

        // then
        assert_eq!(
            err.to_string(),
            "not enough data to decode header version, expected 16 bits, got 10"
        );
    }

    #[test]
    fn complains_if_not_enough_data_is_given_for_header() {
        // given
        let encoded_header = Header::V1(super::HeaderV1 {
            bundle_id: 0,
            num_bits: 0,
            is_last: false,
            idx: 0,
        })
        .encode();

        // when
        let err = Header::decode(&encoded_header[..18]).unwrap_err();

        // then
        assert_eq!(
            err.to_string(),
            "not enough data to decode header, expected 71 bits, got 2"
        );
    }

    #[test]
    fn reports_correct_amount_read() {
        // given
        let mut encoded_header = Header::V1(super::HeaderV1 {
            bundle_id: 0,
            num_bits: 0,
            is_last: false,
            idx: 0,
        })
        .encode();

        // some extra data that should be ignored
        encoded_header.extend_from_bitslice(&bitvec::bitvec![u8, Msb0; 0; 10]);

        // when
        let (_, amount_read) = Header::decode(&encoded_header).unwrap();

        // then
        assert_eq!(amount_read, 87);
    }
}
