use std::num::NonZeroU32;

use super::NonEmptyVec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub data: NonEmptyVec<u8>,
    pub unused_bytes: u32,
    pub total_bytes: NonZeroU32,
}

impl Fragment {
    pub fn utilization(&self) -> f64 {
        self.total_bytes.get().saturating_sub(self.unused_bytes) as f64
            / self.total_bytes.get() as f64
    }
}
