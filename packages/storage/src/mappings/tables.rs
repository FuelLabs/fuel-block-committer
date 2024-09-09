use std::ops::Range;

use ports::types::{
    BlockSubmission, DateTime, StateFragment, StateSubmission, SubmissionTx, TransactionState, Utc,
};
use sqlx::{postgres::PgRow, types::chrono, Row};

macro_rules! bail {
    ($msg: literal, $($args: expr),*) => {
        return Err($crate::error::Error::Conversion(format!($msg, $($args),*)))
    };
}

#[derive(sqlx::FromRow)]
pub struct FuelBlock {
    pub hash: Vec<u8>,
    pub height: i64,
    pub data: Vec<u8>,
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
    pub id: i32,
    pub fuel_block_hash: Vec<u8>,
    pub fuel_block_height: i64,
    pub data: Vec<u8>,
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

        let id = value.id.try_into().map_err(|_| {
            Self::Error::Conversion(format!(
                "Could not convert `id` to u64. Got: {} from db",
                value.id
            ))
        })?;

        Ok(Self {
            id: Some(id),
            block_height,
            block_hash,
            data: value.data,
        })
    }
}

impl From<StateSubmission> for L1StateSubmission {
    fn from(value: StateSubmission) -> Self {
        Self {
            // if not present use placeholder as id is given by db
            id: value.id.unwrap_or_default(),
            fuel_block_height: i64::from(value.block_height),
            fuel_block_hash: value.block_hash.to_vec(),
            data: value.data,
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct L1StateFragment {
    pub id: i32,
    pub submission_id: i64,
    pub start_byte: i32,
    pub end_byte: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<L1StateFragment> for StateFragment {
    type Error = crate::error::Error;

    fn try_from(value: L1StateFragment) -> Result<Self, Self::Error> {
        let start: u32 = value.start_byte.try_into().map_err(|_| {
            Self::Error::Conversion(format!(
                "Could not convert `start_byte` to u32. Got: {} from db",
                value.start_byte
            ))
        })?;

        let end: u32 = value.end_byte.try_into().map_err(|_| {
            Self::Error::Conversion(format!(
                "Could not convert `end_byte` to u32. Got: {} from db",
                value.end_byte
            ))
        })?;

        let range = Range { start, end }.try_into().map_err(|e| {
            Self::Error::Conversion(format!("Db state fragment range validation failed: {e}"))
        })?;

        let submission_id = value.submission_id.try_into().map_err(|_| {
            Self::Error::Conversion(format!(
                "Could not convert `submission_id` to u32. Got: {} from db",
                value.submission_id
            ))
        })?;

        Ok(Self {
            submission_id,
            created_at: value.created_at,
            data_range: range,
        })
    }
}

impl From<StateFragment> for L1StateFragment {
    fn from(value: StateFragment) -> Self {
        Self {
            // We never dictate the ID via StateFragment, db will assign it
            id: Default::default(),
            submission_id: value.submission_id.into(),
            created_at: value.created_at,
            start_byte: value.data_range.as_ref().start.into(),
            end_byte: value.data_range.as_ref().end.into(),
        }
    }
}

pub struct L1SubmissionTx {
    pub id: i64,
    pub hash: Vec<u8>,
    // The fields `state` and `finalized_at` are duplicated in `L1SubmissionTxState` since #[sqlx(flatten)] is not an option because `query_as!` doesn't use `FromRow` and consequently doesn't flatten
    pub state: i16,
    pub finalized_at: Option<DateTime<Utc>>,
}

impl L1SubmissionTx {
    pub fn parse_state(&self) -> Result<TransactionState, crate::error::Error> {
        match (self.state, self.finalized_at) {
            (0, _) => Ok(TransactionState::Pending),
            (1, Some(finalized_at)) => Ok(TransactionState::Finalized(finalized_at)),
            (1, None) => {
                bail!(
                    "L1SubmissionTx(id={}) is missing finalized_at field. Must not happen since there should have been a constraint on the table!", self.id
                )
            }
            (2, _) => Ok(TransactionState::Failed),
            _ => {
                bail!(
                    "L1SubmissionTx(id={}) has invalid state {}",
                    self.id,
                    self.state
                )
            }
        }
    }
}

impl From<SubmissionTx> for L1SubmissionTx {
    fn from(value: SubmissionTx) -> Self {
        let L1SubmissionTxState {
            state,
            finalized_at,
        } = value.state.into();

        Self {
            // if not present use placeholder as id is given by db
            id: value.id.unwrap_or_default() as i64,
            hash: value.hash.to_vec(),
            state,
            finalized_at,
        }
    }
}

impl TryFrom<L1SubmissionTx> for SubmissionTx {
    type Error = crate::error::Error;

    fn try_from(value: L1SubmissionTx) -> Result<Self, Self::Error> {
        let hash = value.hash.as_slice();
        let Ok(hash) = hash.try_into() else {
            bail!("Expected 32 bytes for transaction hash, but got: {hash:?} from db",);
        };
        let state = value.parse_state()?;

        let id = value.id.try_into().map_err(|_| {
            Self::Error::Conversion(format!(
                "Could not convert `id` to u64. Got: {} from db",
                value.id
            ))
        })?;

        Ok(SubmissionTx {
            id: Some(id),
            hash,
            state,
        })
    }
}

impl<'r> sqlx::FromRow<'r, PgRow> for L1SubmissionTx {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let L1SubmissionTxState {
            state,
            finalized_at,
        } = L1SubmissionTxState::from_row(row)?;

        let id = row.try_get("id")?;
        let hash = row.try_get("hash")?;

        Ok(Self {
            id,
            hash,
            state,
            finalized_at,
        })
    }
}

#[derive(sqlx::FromRow)]
pub struct L1SubmissionTxState {
    pub state: i16,
    pub finalized_at: Option<DateTime<Utc>>,
}

impl L1SubmissionTxState {
    pub const PENDING_STATE: i16 = 0;
    pub const FINALIZED_STATE: i16 = 1;
    pub const FAILED_STATE: i16 = 2;
}

impl From<TransactionState> for L1SubmissionTxState {
    fn from(value: TransactionState) -> Self {
        let (state, finalized_at) = match value {
            TransactionState::Pending => (Self::PENDING_STATE, None),
            TransactionState::Finalized(finalized_at) => {
                (Self::FINALIZED_STATE, Some(finalized_at))
            }
            TransactionState::Failed => (Self::FAILED_STATE, None),
        };

        Self {
            state,
            finalized_at,
        }
    }
}
