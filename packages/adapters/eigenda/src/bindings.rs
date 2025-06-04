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
        Queued = 1,
        Encoded = 2,
        GatheringSignatures = 3,
        Complete = 4,
        Failed = 5,
    }

    impl BlobStatus {
        pub fn as_str_name(&self) -> &'static str {
            match self {
                BlobStatus::Unknown => "UNKNOWN",
                BlobStatus::Queued => "QUEUED",
                BlobStatus::Encoded => "ENCODED",
                BlobStatus::GatheringSignatures => "GATHERING_SIGNATURES",
                BlobStatus::Complete => "COMPLETE",
                BlobStatus::Failed => "FAILED",
            }
        }
    }

    impl From<i32> for BlobStatus {
        fn from(value: i32) -> Self {
            match value {
                0 => BlobStatus::Unknown,
                1 => BlobStatus::Queued,
                2 => BlobStatus::Encoded,
                3 => BlobStatus::GatheringSignatures,
                4 => BlobStatus::Complete,
                5 => BlobStatus::Failed,
                _ => BlobStatus::Unknown, // Default case
            }
        }
    }
}

// Re-export for compatibility
pub use disperser::*;
