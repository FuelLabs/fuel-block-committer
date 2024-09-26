use std::num::NonZeroU32;

use ports::types::{DateTime, NonEmpty, NonNegative, TransactionState, Utc};
use sqlx::{postgres::PgRow, Row};

macro_rules! bail {
    ($msg: literal, $($args: expr),*) => {
        return Err($crate::error::Error::Conversion(format!($msg, $($args),*)))
    };
}

#[derive(sqlx::FromRow)]
pub struct L1FuelBlockSubmission {
    pub fuel_block_hash: Vec<u8>,
    pub fuel_block_height: i64,
    pub completed: bool,
    pub submittal_height: i64,
}

impl TryFrom<L1FuelBlockSubmission> for ports::types::BlockSubmission {
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

impl From<ports::types::BlockSubmission> for L1FuelBlockSubmission {
    fn from(value: ports::types::BlockSubmission) -> Self {
        Self {
            fuel_block_hash: value.block_hash.to_vec(),
            fuel_block_height: i64::from(value.block_height),
            completed: value.completed,
            submittal_height: value.submittal_height.into(),
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct BundleFragment {
    pub id: i32,
    pub idx: i32,
    pub bundle_id: i32,
    pub data: Vec<u8>,
    pub unused_bytes: i64,
    pub total_bytes: i64,
}

impl TryFrom<BundleFragment> for ports::storage::BundleFragment {
    type Error = crate::error::Error;

    fn try_from(value: BundleFragment) -> Result<Self, Self::Error> {
        let idx = value.idx.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!(
                "Invalid db `idx` ({}). Reason: {e}",
                value.idx
            ))
        })?;
        let bundle_id = value.bundle_id.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!(
                "Invalid db `bundle_id` ({}). Reason: {e}",
                value.bundle_id
            ))
        })?;

        let data = NonEmpty::collect(value.data).ok_or_else(|| {
            crate::error::Error::Conversion("db fragment data is invalid".to_owned())
        })?;

        let id = value.id.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!("Invalid db `id` ({}). Reason: {e}", value.id))
        })?;

        let unused_bytes: NonNegative<i64> = value.unused_bytes.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!(
                "Invalid db `unused_bytes` ({}). Reason: {e}",
                value.unused_bytes
            ))
        })?;

        let unused_bytes: u32 = unused_bytes.as_u64().try_into().map_err(|e| {
            crate::error::Error::Conversion(format!(
                "Invalid db `unused_bytes` ({}). Reason: {e}",
                value.unused_bytes
            ))
        })?;

        let total_bytes: NonNegative<i64> = value.total_bytes.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!(
                "Invalid db `total_bytes` ({}). Reason: {e}",
                value.total_bytes
            ))
        })?;

        let total_bytes: u32 = total_bytes.as_u64().try_into().map_err(|e| {
            crate::error::Error::Conversion(format!(
                "Invalid db `total_bytes` ({}). Reason: {e}",
                value.total_bytes
            ))
        })?;

        let total_bytes: NonZeroU32 = total_bytes.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!(
                "Invalid db `total_bytes` ({}). Reason: {e}",
                value.total_bytes
            ))
        })?;

        let fragment = ports::types::Fragment {
            data,
            unused_bytes,
            total_bytes,
        };

        Ok(Self {
            id,
            idx,
            bundle_id,
            fragment,
        })
    }
}

#[derive(sqlx::FromRow)]
pub struct FuelBlock {
    pub hash: Vec<u8>,
    pub height: i64,
    pub data: Vec<u8>,
}

impl From<ports::storage::FuelBlock> for FuelBlock {
    fn from(value: ports::storage::FuelBlock) -> Self {
        Self {
            hash: value.hash.to_vec(),
            height: value.height.into(),
            data: value.data.into(),
        }
    }
}

impl TryFrom<FuelBlock> for ports::storage::FuelBlock {
    type Error = crate::error::Error;

    fn try_from(value: FuelBlock) -> Result<Self, Self::Error> {
        let hash = value.hash.as_slice();
        let Ok(block_hash) = hash.try_into() else {
            bail!("Expected 32 bytes for `hash`, but got: {hash:?} from db",);
        };

        let height = value.height.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!(
                "Invalid db `height` ({}). Reason: {e}",
                value.height
            ))
        })?;

        let data = NonEmpty::collect(value.data)
            .ok_or_else(|| crate::error::Error::Conversion("Invalid db `data`.".to_string()))?;

        Ok(Self {
            height,
            hash: block_hash,
            data,
        })
    }
}

pub struct L1Tx {
    pub id: i64,
    pub hash: Vec<u8>,
    // The fields `state` and `finalized_at` are duplicated in `L1SubmissionTxState` since #[sqlx(flatten)] is not an option because `query_as!` doesn't use `FromRow` and consequently doesn't flatten
    pub state: i16,
    pub finalized_at: Option<DateTime<Utc>>,
}

impl L1Tx {
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

impl From<ports::types::L1Tx> for L1Tx {
    fn from(value: ports::types::L1Tx) -> Self {
        let L1TxState {
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

impl TryFrom<L1Tx> for ports::types::L1Tx {
    type Error = crate::error::Error;

    fn try_from(value: L1Tx) -> Result<Self, Self::Error> {
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

        Ok(Self {
            id: Some(id),
            hash,
            state,
        })
    }
}

impl<'r> sqlx::FromRow<'r, PgRow> for L1Tx {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let L1TxState {
            state,
            finalized_at,
        } = L1TxState::from_row(row)?;

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
pub struct L1TxState {
    pub state: i16,
    pub finalized_at: Option<DateTime<Utc>>,
}

impl L1TxState {
    pub const PENDING_STATE: i16 = 0;
    pub const FINALIZED_STATE: i16 = 1;
    pub const FAILED_STATE: i16 = 2;
}

impl From<TransactionState> for L1TxState {
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
