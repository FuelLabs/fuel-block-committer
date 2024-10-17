mod decoder;
mod encoder;
pub use decoder::*;
pub use encoder::*;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BundleV1 {
    pub blocks: Vec<Vec<u8>>,
}

impl std::fmt::Debug for BundleV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex_encoded = self.blocks.iter().map(hex::encode).collect::<Vec<String>>();

        f.debug_struct("BundleV1")
            .field("blocks", &hex_encoded)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Bundle {
    V1(BundleV1),
}
