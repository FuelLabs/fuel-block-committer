// This file is retained for compatibility but is deprecated.
// The bindings have been replaced by the rust-eigenda-client library.
// This file will be removed in a future update.

pub mod common {
    // Stub for compatibility
}

pub mod disperser {
    // Stub for compatibility
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(i32)]
    pub enum BlobStatus {
        Unknown = 0,
        Processing = 1,
        Confirmed = 2,
        Finalized = 3,
        Failed = 4,
    }
    
    impl BlobStatus {
        pub fn as_str_name(&self) -> &'static str {
            match self {
                Self::Unknown => "Unknown",
                Self::Processing => "Processing",
                Self::Confirmed => "Confirmed",
                Self::Finalized => "Finalized",
                Self::Failed => "Failed",
            }
        }
    }
}

// Re-export for compatibility
pub use disperser::*;
