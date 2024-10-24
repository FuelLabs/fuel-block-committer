use std::{io::Write, str::FromStr};

use anyhow::{bail, Context};
use flate2::{write::GzEncoder, Compression};

use super::Bundle;

#[derive(Clone, Debug)]
pub struct Encoder {
    compression_level: Option<Compression>,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new(CompressionLevel::default())
    }
}

impl Encoder {
    pub fn new(level: CompressionLevel) -> Self {
        let level = match level {
            CompressionLevel::Disabled => None,
            CompressionLevel::Min => Some(0),
            CompressionLevel::Level1 => Some(1),
            CompressionLevel::Level2 => Some(2),
            CompressionLevel::Level3 => Some(3),
            CompressionLevel::Level4 => Some(4),
            CompressionLevel::Level5 => Some(5),
            CompressionLevel::Level6 => Some(6),
            CompressionLevel::Level7 => Some(7),
            CompressionLevel::Level8 => Some(8),
            CompressionLevel::Level9 => Some(9),
            CompressionLevel::Max => Some(10),
        };
        Self {
            compression_level: level.map(Compression::new),
        }
    }

    pub fn encode(&self, bundle: Bundle) -> anyhow::Result<Vec<u8>> {
        const VERSION_SIZE: usize = std::mem::size_of::<u16>();

        let Bundle::V1(v1) = bundle;

        let postcard_encoded =
            postcard::to_allocvec(&v1).context("could not postcard encode BundleV1 contents")?;

        let blocks_encoded = self
            .compress(&postcard_encoded)
            .context("could not compress postcard encoded BundleV1")?;
        let mut bundle_data = vec![
            0u8;
            blocks_encoded
                .len()
                .checked_add(VERSION_SIZE)
                .expect("never to encode more than usize::MAX")
        ];

        let version = 1u16;
        bundle_data[..VERSION_SIZE].copy_from_slice(&version.to_be_bytes());

        bundle_data[VERSION_SIZE..].copy_from_slice(&blocks_encoded);

        Ok(bundle_data)
    }

    fn compress(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let Some(compression) = self.compression_level else {
            return Ok(data.to_vec());
        };

        let mut encoder = GzEncoder::new(Vec::new(), compression);
        encoder.write_all(data)?;

        Ok(encoder.finish()?)
    }
}

#[derive(Default, Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum CompressionLevel {
    Disabled,
    Min,
    Level1,
    Level2,
    Level3,
    Level4,
    Level5,
    #[default]
    Level6,
    Level7,
    Level8,
    Level9,
    Max,
}

impl<'a> serde::Deserialize<'a> for CompressionLevel {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'a>,
    {
        let as_string = String::deserialize(deserializer)?;

        Self::from_str(&as_string)
            .map_err(|e| serde::de::Error::custom(format!("Invalid compression level: {e}")))
    }
}

impl FromStr for CompressionLevel {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "min" => Ok(Self::Min),
            "level1" => Ok(Self::Level1),
            "level2" => Ok(Self::Level2),
            "level3" => Ok(Self::Level3),
            "level4" => Ok(Self::Level4),
            "level5" => Ok(Self::Level5),
            "level6" => Ok(Self::Level6),
            "level7" => Ok(Self::Level7),
            "level8" => Ok(Self::Level8),
            "level9" => Ok(Self::Level9),
            "max" => Ok(Self::Max),
            _ => bail!("Invalid compression level: {s}"),
        }
    }
}

#[allow(dead_code)]
impl CompressionLevel {
    #[must_use]
    pub fn levels() -> Vec<Self> {
        vec![
            Self::Disabled,
            Self::Min,
            Self::Level1,
            Self::Level2,
            Self::Level3,
            Self::Level4,
            Self::Level5,
            Self::Level6,
            Self::Level7,
            Self::Level8,
            Self::Level9,
            Self::Max,
        ]
    }
}

#[cfg(test)]
mod test {
    use crate::bundle::{CompressionLevel, Encoder};

    #[test]
    fn can_disable_compression() {
        // given
        let compressor = Encoder::new(CompressionLevel::Disabled);
        let data = vec![1, 2, 3];

        // when
        let compressed = compressor.compress(&data).unwrap();

        // then
        assert_eq!(data, compressed);
    }

    #[test]
    fn all_compression_levels_work() {
        let data = vec![1, 2, 3];
        for level in CompressionLevel::levels() {
            let compressor = Encoder::new(level);
            compressor.compress(&data).unwrap();
        }
    }
}
