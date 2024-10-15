pub mod decoder;
pub mod encoder;

#[derive(Debug, Clone, PartialEq)]
pub struct BlobHeaderV1 {
    pub bundle_id: u32,
    pub size: u32,
    pub is_last: bool,
    pub idx: u32,
}

impl BlobHeaderV1 {
    pub const SIZE: usize = 10;

    pub(crate) fn encode(&self, buffer: &mut BitVec<u8, Lsb0>) {
        buffer.extend_from_bitslice(self.bundle_id.view_bits::<Lsb0>());
        buffer.extend_from_bitslice(&self.size.view_bits::<Lsb0>()[..17]);

        buffer.push(self.is_last);

        buffer.extend_from_bitslice(&self.idx.view_bits::<Lsb0>()[..14]);
    }

    pub(crate) fn decode(data: &BitSlice<u8, Lsb0>) -> Result<(Self, &BitSlice<u8, Lsb0>)> {
        // TODO: check if data is long enough
        let bundle_id = data[0..32].load_le();
        let size = data[32..49].load_le::<u32>();
        let is_last = data[49];
        let idx = data[50..64].load_le::<u32>();

        let header = BlobHeaderV1 {
            size,
            bundle_id,
            is_last,
            idx,
        };
        let remaining_data = &data[64..];

        Ok((header, remaining_data))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlobHeader {
    V1(BlobHeaderV1),
}

impl BlobHeader {
    pub(crate) fn encode(&self, buffer: &mut BitVec<u8, Lsb0>) {
        match self {
            BlobHeader::V1(blob_header_v1) => {
                let version = 1u16;
                buffer.extend_from_bitslice(version.view_bits::<Lsb0>());
                blob_header_v1.encode(buffer);
            }
        }
    }
    pub(crate) fn decode(data: &BitSlice<u8, Lsb0>) -> Result<(Self, &BitSlice<u8, Lsb0>)> {
        // TODO: check boundaries
        let version = data[0..16].load_le::<u16>();
        let remaining_data = &data[16..];
        match version {
            1 => {
                let (header, remaining_data) = BlobHeaderV1::decode(remaining_data)?;
                Ok((BlobHeader::V1(header), remaining_data))
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
use bitvec::{field::BitField, order::Lsb0, slice::BitSlice, vec::BitVec, view::BitView};
