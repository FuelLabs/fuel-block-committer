use crate::Result;
use itertools::Itertools;
use ports::{l1::SubmittableFragments, storage::ValidatedRange};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleProposal {
    pub fragments: SubmittableFragments,
    pub block_heights: ValidatedRange<u32>,
    pub optimal: bool,
}

#[cfg_attr(feature = "test-helpers", mockall::automock)]
#[async_trait::async_trait]
pub trait Bundle {
    async fn propose_bundle(&mut self) -> Result<Option<BundleProposal>>;
}

#[async_trait::async_trait]
pub trait BundlerFactory {
    type Bundler: Bundle + Send;
    async fn build(&self) -> Result<Self::Bundler>;
}

pub mod gas_optimizing;
