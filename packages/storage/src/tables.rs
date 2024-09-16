use ports::types::{
    BlockSubmission, StateFragment, StateSubmission, SubmissionTx, TransactionState,
    TransactionType,
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
    pub tx_id: Option<i32>,
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

        let tx_id = value.tx_id.map(|id| id as u32);

        Ok(Self {
            block_hash,
            block_height,
            tx_id,
            submittal_height,
        })
    }
}

impl From<BlockSubmission> for L1FuelBlockSubmission {
    fn from(value: BlockSubmission) -> Self {
        Self {
            fuel_block_hash: value.block_hash.to_vec(),
            fuel_block_height: i64::from(value.block_height),
            tx_id: value.tx_id.map(|id| id as i32),
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
            id: Some(value.id as u32),
            block_height,
            block_hash,
        })
    }
}

impl From<StateSubmission> for L1StateSubmission {
    fn from(value: StateSubmission) -> Self {
        Self {
            // if not present use placeholder as id is given by db
            id: value.id.unwrap_or_default() as i64,
            fuel_block_height: i64::from(value.block_height),
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
            // if not present use placeholder as id is given by db
            id: value.id.unwrap_or_default() as i64,
            // if not present use placeholder as id is given by db
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
    pub tx_type: i16,
}

impl TryFrom<L1SubmissionTx> for SubmissionTx {
    type Error = crate::error::Error;

    fn try_from(value: L1SubmissionTx) -> Result<Self, Self::Error> {
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

        let Some(tx_type) = TransactionType::from_i16(value.tx_type) else {
            bail!(
                "tx_type: {:?} is not a valid variant of `TransactionType`",
                value.tx_type
            );
        };

        Ok(SubmissionTx {
            id: Some(value.id as u32),
            hash,
            state,
            tx_type,
        })
    }
}

impl From<SubmissionTx> for L1SubmissionTx {
    fn from(value: SubmissionTx) -> Self {
        Self {
            // if not present use placeholder as id is given by db
            id: value.id.unwrap_or_default() as i64,
            hash: value.hash.to_vec(),
            state: value.state.into_i16(),
            tx_type: value.tx_type.into_i16(),
        }
    }
}
