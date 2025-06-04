mod bindings;
mod codec;
mod connector;
mod error;

use bindings::BlobStatus;
pub use connector::*;
use services::types::DispersalStatus;

impl From<BlobStatus> for DispersalStatus {
    fn from(status: BlobStatus) -> Self {
        match status {
            BlobStatus::Unknown | BlobStatus::Encoded | BlobStatus::Queued => {
                DispersalStatus::Processing
            }
            BlobStatus::GatheringSignatures => DispersalStatus::Confirmed,
            BlobStatus::Complete => DispersalStatus::Finalized,
            BlobStatus::Failed => DispersalStatus::Failed,
        }
    }
}
