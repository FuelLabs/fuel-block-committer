mod bindings;
mod codec;
mod connector;
mod error;
mod signer;

use bindings::BlobStatus;
pub use connector::*;
use services::types::DispersalStatus;

impl From<BlobStatus> for DispersalStatus {
    fn from(status: BlobStatus) -> Self {
        match status {
            BlobStatus::Queued | BlobStatus::Encoded | BlobStatus::GatheringSignatures => {
                DispersalStatus::Processing
            }
            // TODO: the V2 API introduced new statuses, BlobStatus::Complete now
            // requires that we check the signer percantage, see BlobStatus::Complete docstring
            BlobStatus::Complete => DispersalStatus::Confirmed,
            BlobStatus::Failed => DispersalStatus::Failed,
            _ => DispersalStatus::Other(status.as_str_name().to_string()),
        }
    }
}
