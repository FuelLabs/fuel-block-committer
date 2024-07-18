use ports::types::{
    BlockSubmission, StateFragment, StateSubmission, StateSubmissionTx, TransactionState,
};
use sqlx::types::chrono;

macro_rules! bail {
    ($msg: literal, $($args: expr),*) => {
        return Err(Self::Error::Conversion(format!($msg, $($args),*)));
    };
}

#[derive(sqlx::FromRow)]
pub struct L1FuelBlockSubmission {
    pub fuel_block_hash: Vec<u8>,
    pub fuel_block_height: i64,
    pub completed: bool,
    pub submittal_height: i64,
}

impl TryFrom<L1FuelBlockSubmission> for BlockSubmission {
    type Error = crate::error::Error;

    fn try_from(value: L1FuelBlockSubmission) -> Result<Self, Self::Error> {
        let block_hash = value.fuel_block_hash.as_slice();
        let Ok(block_hash) = block_hash.try_into() else {
            bail!("Expected 32 bytes for `fuel_block_hash`, but got: {block_hash:?} from db",);
        };

        let Ok(block_height) = value.fuel_block_height.try_into() else {
            bail!(
                "`fuel_block_height` as read from the db cannot fit in a `u32` as expected. Got: {:?} from db",
                value.fuel_block_height

            );
        };

        let Ok(submittal_height) = value.submittal_height.try_into() else {
            bail!("`submittal_height` as read from the db cannot fit in a `u64` as expected. Got: {} from db", value.submittal_height);
        };

        Ok(Self {
            block_hash,
            block_height,
            completed: value.completed,
            submittal_height,
        })
    }
}

impl From<BlockSubmission> for L1FuelBlockSubmission {
    fn from(value: BlockSubmission) -> Self {
        Self {
            fuel_block_hash: value.block_hash.to_vec(),
            fuel_block_height: i64::from(value.block_height),
            completed: value.completed,
            submittal_height: value.submittal_height.into(),
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct L1StateSubmission {
    pub fuel_block_hash: Vec<u8>,
    pub fuel_block_height: i64,
    pub completed: bool,
}

#[derive(sqlx::FromRow)]
pub struct L1StateFragment {
    pub fuel_block_hash: Vec<u8>,
    pub transaction_hash: Option<Vec<u8>>,
    pub raw_data: Vec<u8>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub fragment_index: i64,
    pub completed: bool,
}

impl TryFrom<L1StateSubmission> for StateSubmission {
    type Error = crate::error::Error;

    fn try_from(value: L1StateSubmission) -> Result<Self, Self::Error> {
        let block_hash = value.fuel_block_hash.as_slice();
        let Ok(block_hash) = block_hash.try_into() else {
            bail!("Expected 32 bytes for `fuel_block_hash`, but got: {block_hash:?} from db",);
        };

        let Ok(block_height) = value.fuel_block_height.try_into() else {
            bail!(
                "`fuel_block_height` as read from the db cannot fit in a `u32` as expected. Got: {:?} from db",
                value.fuel_block_height

            );
        };

        Ok(Self {
            block_height,
            block_hash,
            completed: value.completed,
        })
    }
}

impl From<StateSubmission> for L1StateSubmission {
    fn from(value: StateSubmission) -> Self {
        Self {
            fuel_block_height: i64::from(value.block_height),
            completed: value.completed,
            fuel_block_hash: value.block_hash.to_vec(),
        }
    }
}

impl TryFrom<L1StateFragment> for StateFragment {
    type Error = crate::error::Error;

    fn try_from(value: L1StateFragment) -> Result<Self, Self::Error> {
        let block_hash = value.fuel_block_hash.as_slice();
        let Ok(block_hash) = block_hash.try_into() else {
            bail!("Expected 32 bytes for `fuel_block_hash`, but got: {block_hash:?} from db",);
        };

        let transaction_hash = match value.transaction_hash {
            Some(hash) => {
                let Ok(hash) = hash.as_slice().try_into() else {
                    bail!("Expected 32 bytes for `transaction_hash`, but got: {hash:?} from db",);
                };

                Some(hash)
            }
            None => None,
        };

        let fragment_index = value.fragment_index.try_into();
        let Ok(fragment_index) = fragment_index else {
            bail!(
                "`fragment_index` as read from the db cannot fit in a `u32` as expected. Got: {} from db",
                value.fragment_index
            );
        };

        Ok(Self {
            block_hash,
            transaction_hash,
            raw_data: value.raw_data,
            created_at: value.created_at,
            completed: value.completed,
            fragment_index,
        })
    }
}

impl From<StateFragment> for L1StateFragment {
    fn from(value: StateFragment) -> Self {
        Self {
            fuel_block_hash: value.block_hash.to_vec(),
            transaction_hash: value.transaction_hash.map(|hash| hash.to_vec()),
            raw_data: value.raw_data,
            completed: value.completed,
            fragment_index: i64::from(value.fragment_index),
            created_at: value.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct L1StateSubmissionTx {
    pub hash: Vec<u8>,
    pub state: i16,
}

impl TryFrom<L1StateSubmissionTx> for StateSubmissionTx {
    type Error = crate::error::Error;

    fn try_from(value: L1StateSubmissionTx) -> Result<Self, Self::Error> {
        let hash = value.hash.as_slice();
        let Ok(hash) = hash.try_into() else {
            bail!("Expected 32 bytes for transaction hash, but got: {hash:?} from db",);
        };

        let Some(state) = TransactionState::from_i16(value.state) else {
            bail!(
                "state: {:?} is not a valid variant of `TransactionState`",
                value.state
            );
        };

        Ok(StateSubmissionTx { hash, state })
    }
}

impl From<StateSubmissionTx> for L1StateSubmissionTx {
    fn from(value: StateSubmissionTx) -> Self {
        Self {
            hash: value.hash.to_vec(),
            state: value.state.into_i16(),
        }
    }
}
