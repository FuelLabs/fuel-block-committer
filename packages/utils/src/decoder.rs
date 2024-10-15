use bitvec::{order::Lsb0, slice::BitSlice};

use crate::{Blob, BlobHeader, BlobHeaderV1, BlobWithProof};

pub struct NewDecoder {}
impl NewDecoder {
    pub fn decode(&self, blobs: &[Blob]) -> anyhow::Result<Vec<u8>> {
        todo!()
        // let data = blobs[0].blob;
        //
        // let header = self.read_header(&data)?;
    }

    pub fn read_header(&self, blob: &Blob) -> anyhow::Result<BlobHeader> {
        let buffer = BitSlice::<u8, Lsb0>::from_slice(blob.as_slice());

        let (header, _) = BlobHeader::decode(buffer)?;

        Ok(header)
    }
}
