use std::{io::Read, marker::PhantomData};

use anyhow::Context;
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

        let data = Self::decompress(&data[2..])
            .with_context(|| "failed to decompress BundleV1 contents")?;

        let blocks: BundleV1 = postcard::from_bytes(&data)
            .with_context(|| "failed to postcard decode decompressed contents of BundleV1")?;

        Ok(super::Bundle::V1(blocks))
    }

    fn decompress(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut decoder = GzDecoder::new(data);

        let mut buf = vec![];
        decoder.read_to_end(&mut buf)?;

        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use crate::bundle::{Bundle, BundleV1, Encoder};

    #[test]
    fn complains_about_unsupported_version() {
        // given
        let encoder = Encoder::default();
        let mut encoded_bundle = encoder
            .encode(Bundle::V1(BundleV1 { blocks: vec![] }))
            .unwrap();
        encoded_bundle[..2].copy_from_slice(&5u16.to_be_bytes());
        let decoder = super::Decoder::default();

        // when
        let err = decoder.decode(&encoded_bundle).unwrap_err();

        // then
        let expected = "Unsupported bundle version: 5";
        assert_eq!(err.to_string(), expected);
    }

    #[test]
    fn complains_about_not_enough_data() {
        // given
        let decoder = super::Decoder::default();

        // when
        let err = decoder.decode(&[1]).unwrap_err();

        // then
        assert_eq!(err.to_string(), "Bundle data too short to contain version");
    }
}
