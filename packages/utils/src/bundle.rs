#[derive(Debug, Clone, PartialEq)]
pub struct BundleV1 {
    pub blocks: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Bundle {
    V1(BundleV1),
}
