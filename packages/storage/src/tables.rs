use ports::types::{
    BlockSubmission, StateFragment, StateSubmission, SubmissionTx, TransactionState,
};
use sqlx::types::chrono;

macro_rules! bail {
    ($msg:literal, $($args:expr),*) => {
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
        let block_hash: [u8; 32] = value
            .fuel_block_hash
            .try_into()
            .map_err(|_| {
                crate::error::Error::Conversion(format!(
                    "Expected 32 bytes for `fuel_block_hash`, but got: {:?} from db",
                    value.fuel_block_hash
                ))
            })?;

        let block_height = u32::try_from(value.fuel_block_height).map_err(|_| {
            crate::error::Error::Conversion(format!(
                "`fuel_block_height` from db cannot fit in a `u32`. Got: {:?}",
                value.fuel_block_height
            ))
        })?;

        let submittal_height = u64::try_from(value.submittal_height).map_err(|_| {
            crate::error::Error::Conversion(format!(
                "`submittal_height` from db cannot fit in a `u64`. Got: {:?}",
                value.submittal_height
            ))
        })?;

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
            fuel_block_height: value.block_height.into(),
            completed: value.completed,
            submittal_height: value.submittal_height.into(),
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct L1StateSubmission {
    pub id: i64,
    pub fuel_block_hash: Vec<u8>,
    pub fuel_block_height: i64,
}

impl TryFrom<L1StateSubmission> for StateSubmission {
    type Error = crate::error::Error;

    fn try_from(value: L1StateSubmission) -> Result<Self, Self::Error> {
        let block_hash: [u8; 32] = value
            .fuel_block_hash
            .try_into()
            .map_err(|_| {
                crate::error::Error::Conversion(format!(
                    "Expected 32 bytes for `fuel_block_hash`, but got: {:?} from db",
                    value.fuel_block_hash
                ))
            })?;

        let block_height = u32::try_from(value.fuel_block_height).map_err(|_| {
            crate::error::Error::Conversion(format!(
                "`fuel_block_height` from db cannot fit in a `u32`. Got: {:?}",
                value.fuel_block_height
            ))
        })?;

        Ok(Self {
            id: Some(value.id as u32),
            block_height,
            block_hash,
        })
    }
}

impl From<StateSubmission> for L1StateSubmission {
    fn from(value: StateSubmission) -> Self {
        Self {
            id: value.id.unwrap_or_default() as i64,
            fuel_block_height: value.block_height.into(),
            fuel_block_hash: value.block_hash.to_vec(),
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct L1StateFragment {
    pub id: i64,
    pub submission_id: i64,
    pub fragment_idx: i64,
    pub data: Vec<u8>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<L1StateFragment> for StateFragment {
    type Error = crate::error::Error;

    fn try_from(value: L1StateFragment) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Some(value.id as u32),
            submission_id: Some(value.submission_id as u32),
            fragment_idx: value.fragment_idx as u32,
            data: value.data,
            created_at: value.created_at,
        })
    }
}

impl From<StateFragment> for L1StateFragment {
    fn from(value: StateFragment) -> Self {
        Self {
            id: value.id.unwrap_or_default() as i64,
            submission_id: value.submission_id.unwrap_or_default() as i64,
            fragment_idx: value.fragment_idx as i64,
            data: value.data,
            created_at: value.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct L1SubmissionTx {
    pub id: i64,
    pub hash: Vec<u8>,
    pub state: i16,
}

impl TryFrom<L1SubmissionTx> for SubmissionTx {
    type Error = crate::error::Error;

    fn try_from(value: L1SubmissionTx) -> Result<Self, Self::Error> {
        let hash: [u8; 32] = value
            .hash
            .try_into()
            .map_err(|_| {
                crate::error::Error::Conversion(format!(
                    "Expected 32 bytes for transaction hash, but got: {:?} from db",
                    value.hash
                ))
            })?;

        let state = TransactionState::from_i16(value.state).ok_or_else(|| {
            crate::error::Error::Conversion(format!(
                "state: {:?} is not a valid variant of `TransactionState`",
                value.state
            ))
        })?;

        Ok(SubmissionTx {
            id: Some(value.id as u32),
            hash,
            state,
        })
    }
}

impl From<SubmissionTx> for L1SubmissionTx {
    fn from(value: SubmissionTx) -> Self {
        Self {
            id: value.id.unwrap_or_default() as i64,
            hash: value.hash.to_vec(),
            state: value.state.into_i16(),
        }
    }
}
