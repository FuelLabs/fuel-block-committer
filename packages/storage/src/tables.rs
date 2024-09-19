use num_bigint::BigInt;
use ports::types::{
    BlockSubmission, BlockSubmissionTx, DateTime, StateFragment, StateSubmission, SubmissionTx,
    TransactionState, Utc,
};
use sqlx::types::{chrono, BigDecimal};

macro_rules! bail {
    ($msg: literal, $($args: expr),*) => {
        return Err(Self::Error::Conversion(format!($msg, $($args),*)));
    };
}

#[derive(sqlx::FromRow)]
pub struct L1FuelBlockSubmission {
    pub id: i64,
    pub fuel_block_hash: Vec<u8>,
    pub fuel_block_height: i64,
    pub final_tx_id: Option<i32>,
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

        let tx_id = value.final_tx_id.map(|id| id as u32);

        Ok(Self {
            id: Some(value.id as u32),
            block_hash,
            block_height,
            final_tx_id: tx_id,
        })
    }
}

impl From<BlockSubmission> for L1FuelBlockSubmission {
    fn from(value: BlockSubmission) -> Self {
        Self {
            id: value.id.unwrap_or_default() as i64,
            fuel_block_hash: value.block_hash.to_vec(),
            fuel_block_height: i64::from(value.block_height),
            final_tx_id: value.final_tx_id.map(|id| id as i32),
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct L1FuelBlockSubmissionTx {
    pub id: i64,
    pub submission_id: i64,
    pub hash: Vec<u8>,
    pub nonce: i64,
    pub max_fee: BigDecimal,
    pub priority_fee: BigDecimal,
    pub state: i16,
    pub created_at: DateTime<Utc>,
}

// Assumes that the BigDecimal is non-negative and has no fractional part
fn bigdecimal_to_u128(value: BigDecimal) -> Result<u128, crate::error::Error> {
    let (digits, _scale) = value.into_bigint_and_exponent();
    let result: u128 = digits
        .try_into()
        .map_err(|_| crate::error::Error::Conversion("Digits exceed u128 range".to_string()))?;

    Ok(result)
}

fn u128_to_bigdecimal(value: u128) -> BigDecimal {
    let digits = BigInt::from(value);
    BigDecimal::new(digits, 0)
}

impl TryFrom<L1FuelBlockSubmissionTx> for BlockSubmissionTx {
    type Error = crate::error::Error;

    fn try_from(value: L1FuelBlockSubmissionTx) -> Result<Self, Self::Error> {
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

        Ok(Self {
            id: Some(value.id as u32),
            submission_id: Some(value.submission_id as u32),
            hash,
            nonce: value.nonce as u32,
            max_fee: bigdecimal_to_u128(value.max_fee)?,
            priority_fee: bigdecimal_to_u128(value.priority_fee)?,
            state,
            created_at: value.created_at,
        })
    }
}

impl From<BlockSubmissionTx> for L1FuelBlockSubmissionTx {
    fn from(value: BlockSubmissionTx) -> Self {
        let max_fee = u128_to_bigdecimal(value.max_fee);
        let priority_fee = u128_to_bigdecimal(value.priority_fee);

        Self {
            id: value.id.unwrap_or_default() as i64,
            submission_id: value.submission_id.unwrap_or_default() as i64,
            hash: value.hash.to_vec(),
            nonce: value.nonce as i64,
            max_fee,
            priority_fee,
            state: value.state.into_i16(),
            created_at: value.created_at,
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
            // if not present use placeholder as id is given by db
            id: value.id.unwrap_or_default() as i64,
            hash: value.hash.to_vec(),
            state: value.state.into_i16(),
        }
    }
}
