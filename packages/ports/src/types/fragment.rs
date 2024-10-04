use std::num::NonZeroU32;

use crate::types::NonEmpty;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub data: NonEmpty<u8>,
    pub unused_bytes: u32,
    pub total_bytes: NonZeroU32,
}

impl Fragment {
    pub fn utilization(&self) -> f64 {
        f64::from(self.total_bytes.get().saturating_sub(self.unused_bytes))
            / f64::from(self.total_bytes.get())
    }
}
