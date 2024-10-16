pub mod decoder;
pub mod encoder;

#[derive(Debug, Clone, PartialEq)]
pub struct BlobHeaderV1 {
    pub bundle_id: u32,
    /// Number of bits containing data
    pub num_bits: u32,
    pub is_last: bool,
    pub idx: u32,
}

impl BlobHeaderV1 {
    const BUNDLE_ID_BITS: usize = 32;
    const NUM_BITS_BITS: usize = 21;
    const IS_LAST_BITS: usize = 1;
    const IDX_SIZE_BITS: usize = 17;
    pub const TOTAL_SIZE_BITS: usize =
        Self::BUNDLE_ID_BITS + Self::NUM_BITS_BITS + Self::IS_LAST_BITS + Self::IDX_SIZE_BITS;

    pub(crate) fn encode(&self, buffer: &mut BitVec<u8, Msb0>) {
        buffer.extend_from_bitslice(self.bundle_id.view_bits::<Msb0>());

        buffer.extend_from_bitslice(&self.num_bits.view_bits::<Msb0>()[32 - Self::NUM_BITS_BITS..]);

        buffer.push(self.is_last);

        buffer.extend_from_bitslice(&self.idx.view_bits::<Msb0>()[32 - Self::IDX_SIZE_BITS..]);
    }

    pub(crate) fn decode(data: &BitSlice<u8, Msb0>) -> Result<(Self, usize)> {
        // TODO: check if data is long enough
        let bundle_id = data[..Self::BUNDLE_ID_BITS].load_be();
        let remaining_data = &data[Self::BUNDLE_ID_BITS..];

        let num_bits = remaining_data[..Self::NUM_BITS_BITS].load_be::<u32>();
        let remaining_data = &remaining_data[Self::NUM_BITS_BITS..];

        let is_last = remaining_data[0];
        let remaining_data = &remaining_data[1..];

        let idx = remaining_data[..Self::IDX_SIZE_BITS].load_be::<u32>();
        let remaining_data = &remaining_data[Self::IDX_SIZE_BITS..];

        let header = BlobHeaderV1 {
            num_bits,
            bundle_id,
            is_last,
            idx,
        };

        let amount_read = data.len().saturating_sub(remaining_data.len());

        Ok((header, amount_read))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlobHeader {
    V1(BlobHeaderV1),
}

impl BlobHeader {
    const VERSION_BITS: usize = 16;
    pub const V1_SIZE_BITS: usize = BlobHeaderV1::TOTAL_SIZE_BITS + Self::VERSION_BITS;

    pub(crate) fn encode(&self) -> BitVec<u8, Msb0> {
        match self {
            BlobHeader::V1(blob_header_v1) => {
                let mut buffer = BitVec::<u8, Msb0>::new();

                let version = 1u16;
                buffer.extend_from_bitslice(version.view_bits::<Msb0>());

                blob_header_v1.encode(&mut buffer);

                buffer
            }
        }
    }
    pub(crate) fn decode(data: &BitSlice<u8, Msb0>) -> Result<(Self, usize)> {
        // TODO: check boundaries
        let version = data[..Self::VERSION_BITS].load_be::<u16>();
        // eprintln!("version: {:?}", version);

        let remaining_data = &data[Self::VERSION_BITS..];
        let read_version_bits = data.len() - remaining_data.len();
        match version {
            1 => {
                let (header, read_bits) = BlobHeaderV1::decode(remaining_data)?;
                Ok((BlobHeader::V1(header), read_bits + read_version_bits))
            }
            version => bail!("Unsupported version {version}"),
        }
    }
}

pub type Blob = Box<[u8; BYTES_PER_BLOB]>;

#[derive(Debug, Clone, PartialEq)]
pub struct BlobWithProof {
    // needs to be heap allocated because it's large enough to cause a stack overflow
    pub blob: Blob,
    pub commitment: [u8; 48],
    pub proof: [u8; 48],
}

pub use alloy::consensus::Bytes48;
use alloy::eips::eip4844::BYTES_PER_BLOB;
use anyhow::{bail, Result};
use bitvec::{
    field::BitField,
    order::{Lsb0, Msb0},
    slice::BitSlice,
    vec::BitVec,
    view::BitView,
};
