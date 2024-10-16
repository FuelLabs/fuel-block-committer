use std::{io::Read, marker::PhantomData};

use flate2::read::GzDecoder;

use crate::bundle::BundleV1;

#[derive(Clone, Debug, Default)]
pub struct Decoder {
    private: PhantomData<()>,
}

impl Decoder {
    pub fn decode(&self, data: &[u8]) -> anyhow::Result<super::Bundle> {
        if data.len() < 2 {
            anyhow::bail!("Bundle data too short to contain version");
        }
        let version = u16::from_be_bytes([data[0], data[1]]);
        if version != 1 {
            anyhow::bail!("Unsupported bundle version: {version}");
        }

        eprintln!("about to decompress");
        let data = self.decompress(&data[2..])?;
        eprintln!("managed to decompress");

        let blocks: BundleV1 = postcard::from_bytes(&data)?;

        Ok(super::Bundle::V1(blocks))
    }

    // TODO: private
    pub fn decompress(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut decoder = GzDecoder::new(data);

        let mut buf = vec![];
        decoder.read_to_end(&mut buf)?;

        Ok(buf)
    }
}
