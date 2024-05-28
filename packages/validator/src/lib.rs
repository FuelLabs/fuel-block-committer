pub mod block;
#[cfg(feature = "validator")]
mod validator;

use fuel_core_client::client::types::block::Block as FuelBlock;
#[cfg(feature = "validator")]
pub use validator::*;

use crate::block::ValidatedFuelBlock;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    BlockValidation(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg_attr(feature = "test-helpers", mockall::automock)]
pub trait Validator: Send + Sync {
    fn validate(&self, fuel_block: &FuelBlock) -> Result<ValidatedFuelBlock>;
}
