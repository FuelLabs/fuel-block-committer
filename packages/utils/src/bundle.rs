#[derive(Debug, Clone, PartialEq)]
pub struct BundleV1 {
    pub blocks: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Bundle {
    V1(BundleV1),
}

mod encoder {
    use std::{io::Write, str::FromStr};

    use anyhow::bail;
    use flate2::write::GzEncoder;

    #[derive(Clone, Debug, Default)]
    pub struct Encoder {
        compression_level: CompressionLevel,
    }

    impl Encoder {
        pub fn new(level: CompressionLevel) -> Self {
            Self {
                compression_level: level,
            }
        }

        fn compress(&self, data: Vec<u8>) -> anyhow::Result<Vec<u8>> {
            let bytes = Vec::from(data);

            let mut encoder = GzEncoder::new(Vec::new(), self.compression_level.into());
            encoder
                .write_all(&bytes)
                .map_err(|e| crate::Error::Other(e.to_string()))?;

            encoder
                .finish()
                .map_err(|e| crate::Error::Other(e.to_string()))?
                .into_iter()
                .collect_nonempty()
                .ok_or_else(|| crate::Error::Other("compression resulted in no data".to_string()))
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

            CompressionLevel::from_str(&as_string)
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
}
mod decoder {}
