use crate::adapters::{
    fuel_adapter::FuelBlock,
    storage::{BlockSubmission, Error},
};

#[derive(sqlx::FromRow)]
pub struct EthFuelBlockSubmission {
    pub fuel_block_hash: Vec<u8>,
    pub fuel_block_height: i64,
    pub completed: bool,
    pub submittal_height: i64,
}

impl TryFrom<EthFuelBlockSubmission> for BlockSubmission {
    type Error = Error;

    fn try_from(value: EthFuelBlockSubmission) -> Result<Self, Self::Error> {
        let block_hash = value.fuel_block_hash.as_slice();
        macro_rules! bail {
            ($msg: literal, $($args: expr),*) => {
                return Err(Error::Conversion(format!($msg, $($args),*)));
            };
        }
        let Ok(hash) = block_hash.try_into() else {
            bail!("Expected 32 bytes for `fuel_block_hash`, but got: {block_hash:?} from db",);
        };

        let Ok(height) = value.fuel_block_height.try_into() else {
            bail!(
                "`fuel_block_height` as read from the db cannot fit in a `u32` as expected. Got: {:?} from db",
                value.fuel_block_height

            );
        };

        let Ok(submittal_height) = value.submittal_height.try_into() else {
            bail!("`submittal_height` as read from the db cannot fit in a `u64` as expected. Got: {} from db", value.submittal_height);
        };

        Ok(Self {
            block: FuelBlock { hash, height },
            completed: value.completed,
            submittal_height,
        })
    }
}

impl From<BlockSubmission> for EthFuelBlockSubmission {
    fn from(value: BlockSubmission) -> Self {
        Self {
            fuel_block_hash: value.block.hash.to_vec(),
            fuel_block_height: i64::from(value.block.height),
            completed: value.completed,
            submittal_height: value.submittal_height.into(),
        }
    }
}
