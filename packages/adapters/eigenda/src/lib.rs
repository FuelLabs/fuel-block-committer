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
            BlobStatus::Processing => DispersalStatus::Processing,
            BlobStatus::Confirmed => DispersalStatus::Confirmed,
            BlobStatus::Finalized => DispersalStatus::Finalized,
            // TODO find a way to at least log a strange status
            _ => DispersalStatus::Failed,
        }
    }
}
