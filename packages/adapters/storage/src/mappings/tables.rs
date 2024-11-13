use std::num::NonZeroU32;

use num_bigint::BigInt;
use services::types::{
    BlockSubmissionTx, CompressedFuelBlock, DateTime, NonEmpty, NonNegative, TransactionState, Utc,
};
use sqlx::types::BigDecimal;

macro_rules! bail {
    ($msg: literal, $($args: expr),*) => {
        return Err($crate::error::Error::Conversion(format!($msg, $($args),*)))
    };
}

#[derive(sqlx::FromRow)]
pub struct L1FuelBlockSubmission {
    pub id: i32,
    pub fuel_block_hash: Vec<u8>,
    pub fuel_block_height: i64,
    pub completed: bool,
}

impl TryFrom<L1FuelBlockSubmission> for services::types::BlockSubmission {
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

        let Ok(id) = NonNegative::try_from(value.id) else {
            bail!(
                "Expected a non-negative `id`, but got: {} from db",
                value.id
            );
        };

        Ok(Self {
            id: Some(id),
            block_hash,
            block_height,
            completed: value.completed,
        })
    }
}

impl From<services::types::BlockSubmission> for L1FuelBlockSubmission {
    fn from(value: services::types::BlockSubmission) -> Self {
        Self {
            id: value.id.unwrap_or_default().as_i32(),
            fuel_block_hash: value.block_hash.to_vec(),
            fuel_block_height: i64::from(value.block_height),
            completed: value.completed,
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct L1FuelBlockSubmissionTx {
    pub id: i32,
    pub submission_id: i32,
    pub hash: Vec<u8>,
    pub nonce: i64,
    pub max_fee: BigDecimal,
    pub priority_fee: BigDecimal,
    pub state: i16,
    pub created_at: Option<DateTime<Utc>>,
    pub finalized_at: Option<DateTime<Utc>>,
}

impl L1FuelBlockSubmissionTx {
    pub fn parse_state(&self) -> Result<TransactionState, crate::error::Error> {
        match (self.state, self.finalized_at) {
            (0, _) => Ok(TransactionState::Pending),
            (1, Some(finalized_at)) => Ok(TransactionState::Finalized(finalized_at)),
            (1, None) => {
                bail!(
                    "L1FuelBlockSubmissionTx(id={}) is missing finalized_at field. Must not happen since there should have been a constraint on the table!", self.id
                )
            }
            (2, _) => Ok(TransactionState::Failed),
            _ => {
                bail!(
                    "L1FuelBlockSubmissionTx(id={}) has invalid state {}",
                    self.id,
                    self.state
                )
            }
        }
    }
}

// Assumes that the BigDecimal is non-negative and has no fractional part
fn bigdecimal_to_u128(value: BigDecimal) -> Result<u128, crate::error::Error> {
    let (digits, scale) = value.clone().into_bigint_and_exponent();

    if scale > 0 {
        return Err(crate::error::Error::Conversion(format!(
            "Expected whole number, got fractual from db: {value}"
        )));
    }

    let result: u128 = digits
        .try_into()
        .map_err(|_| crate::error::Error::Conversion("Digits exceed u128 range".to_string()))?;

    let result = result.saturating_mul(10u128.saturating_pow(scale.unsigned_abs() as u32));

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

        let state = value.parse_state()?;
        let id = NonNegative::try_from(value.id).map_err(|_| {
            Self::Error::Conversion(format!(
                "Could not convert `id` to NonNegative<i32>. Got: {} from db",
                value.id
            ))
        })?;

        let submission_id = NonNegative::try_from(value.submission_id).map_err(|_| {
            Self::Error::Conversion(format!(
                "Could not convert `submission_id` to NonNegative<i32>. Got: {} from db",
                value.submission_id
            ))
        })?;

        Ok(Self {
            id: Some(id),
            submission_id: Some(submission_id),
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

        let state = L1TxState::from(&value.state).into();
        let finalized_at = match value.state {
            TransactionState::Finalized(finalized_at) => Some(finalized_at),
            _ => None,
        };

        Self {
            id: value.id.unwrap_or_default().as_i32(),
            submission_id: value.submission_id.unwrap_or_default().as_i32(),
            hash: value.hash.to_vec(),
            nonce: value.nonce as i64,
            max_fee,
            priority_fee,
            state,
            finalized_at,
            created_at: value.created_at,
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

impl TryFrom<BundleFragment> for services::ports::storage::BundleFragment {
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

        let fragment = services::types::Fragment {
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
pub(crate) struct DBCompressedFuelBlock {
    pub height: i64,
    pub data: Vec<u8>,
}

impl From<CompressedFuelBlock> for DBCompressedFuelBlock {
    fn from(value: CompressedFuelBlock) -> Self {
        Self {
            height: value.height.into(),
            data: value.data.into(),
        }
    }
}

impl TryFrom<DBCompressedFuelBlock> for CompressedFuelBlock {
    type Error = crate::error::Error;

    fn try_from(value: DBCompressedFuelBlock) -> Result<Self, Self::Error> {
        let height = value.height.try_into().map_err(|e| {
            crate::error::Error::Conversion(format!(
                "Invalid db `height` ({}). Reason: {e}",
                value.height
            ))
        })?;

        let data = NonEmpty::collect(value.data)
            .ok_or_else(|| crate::error::Error::Conversion("Invalid db `data`.".to_string()))?;

        Ok(Self { height, data })
    }
}

#[derive(sqlx::FromRow)]
pub struct L1Tx {
    pub id: i32,
    pub hash: Vec<u8>,
    pub nonce: i64,
    pub max_fee: BigDecimal,
    pub priority_fee: BigDecimal,
    pub blob_fee: BigDecimal,
    pub created_at: Option<DateTime<Utc>>,
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
            (3, _) => Ok(TransactionState::IncludedInBlock),
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

impl From<services::types::L1Tx> for L1Tx {
    fn from(value: services::types::L1Tx) -> Self {
        let max_fee = u128_to_bigdecimal(value.max_fee);
        let priority_fee = u128_to_bigdecimal(value.priority_fee);
        let blob_fee = u128_to_bigdecimal(value.blob_fee);

        let state = L1TxState::from(&value.state).into();
        let finalized_at = match value.state {
            TransactionState::Finalized(finalized_at) => Some(finalized_at),
            _ => None,
        };

        Self {
            // if not present use placeholder as id is given by db
            id: value.id.unwrap_or_default() as i32,
            hash: value.hash.to_vec(),
            state,
            finalized_at,
            nonce: value.nonce as i64,
            max_fee,
            priority_fee,
            blob_fee,
            created_at: value.created_at,
        }
    }
}

impl TryFrom<L1Tx> for services::types::L1Tx {
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
            nonce: value.nonce as u32,
            max_fee: bigdecimal_to_u128(value.max_fee)?,
            priority_fee: bigdecimal_to_u128(value.priority_fee)?,
            blob_fee: bigdecimal_to_u128(value.blob_fee)?,
            created_at: value.created_at,
        })
    }
}

pub enum L1TxState {
    Pending,
    Finalized,
    Failed,
    IncludedInBlock,
}

impl From<L1TxState> for i16 {
    fn from(value: L1TxState) -> Self {
        match value {
            L1TxState::Pending => 0,
            L1TxState::Finalized => 1,
            L1TxState::Failed => 2,
            L1TxState::IncludedInBlock => 3,
        }
    }
}

impl From<&TransactionState> for L1TxState {
    fn from(value: &TransactionState) -> Self {
        match value {
            TransactionState::Pending => Self::Pending,
            TransactionState::IncludedInBlock => Self::IncludedInBlock,
            TransactionState::Finalized(_) => Self::Finalized,
            TransactionState::Failed => Self::Failed,
        }
    }
}
