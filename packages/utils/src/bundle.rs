mod decoder;
mod encoder;
pub use decoder::*;
pub use encoder::*;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BundleV1 {
    pub blocks: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Bundle {
    V1(BundleV1),
}
