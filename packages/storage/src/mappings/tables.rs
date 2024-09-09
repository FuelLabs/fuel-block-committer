use ports::types::{DateTime, TransactionState, Utc};
use sqlx::{postgres::PgRow, Row};

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

impl From<ports::storage::FuelBlock> for FuelBlock {
    fn from(value: ports::storage::FuelBlock) -> Self {
        Self {
            hash: value.hash.to_vec(),
            height: value.height.into(),
            data: value.data,
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

        Ok(Self {
            height,
            hash: block_hash,
            data: value.data,
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
