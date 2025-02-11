use std::cmp::max;

use alloy::{
    network::{TransactionBuilder, TransactionBuilder4844},
    providers::utils::EIP1559_MIN_PRIORITY_FEE,
    rpc::types::{FeeHistory, TransactionRequest},
};
use itertools::Itertools;
use services::types::EthereumDetails;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct MaxTxFeesPerGas {
    pub normal: u128,
    pub priority: u128,
    pub blob: u128,
}

impl<'a> From<&'a EthereumDetails> for MaxTxFeesPerGas {
    fn from(value: &'a EthereumDetails) -> Self {
        Self {
            normal: value.max_fee,
            priority: value.blob_fee,
            blob: value.priority_fee,
        }
    }
}

impl MaxTxFeesPerGas {
    pub fn double(self) -> Self {
        Self {
            normal: self.normal.saturating_mul(2),
            priority: self.priority.saturating_mul(2),
            blob: self.blob.saturating_mul(2),
        }
    }

    pub fn retain_max(self, previous_fees: MaxTxFeesPerGas) -> Self {
        Self {
            normal: max(self.normal, previous_fees.normal),
            priority: max(self.priority, previous_fees.priority),
            blob: max(self.blob, previous_fees.blob),
        }
    }
}

impl TryFrom<FeeHistory> for MaxTxFeesPerGas {
    type Error = crate::error::Error;

    fn try_from(fee_history: FeeHistory) -> std::result::Result<Self, Self::Error> {
        let blob = fee_history.latest_block_blob_base_fee().ok_or_else(|| {
            crate::error::Error::Other("blob base fee not found in fee history".to_string())
        })?;

        let normal = fee_history.latest_block_base_fee().ok_or_else(|| {
            crate::error::Error::Other("base fee not found in fee history".to_string())
        })?;

        let priority = estimate_max_priority_fee_per_gas(&fee_history)?;

        Ok(MaxTxFeesPerGas {
            priority,
            normal,
            blob,
        })
    }
}

pub trait TransactionRequestExt {
    fn with_max_fees(self, fees: MaxTxFeesPerGas) -> Self;
}

impl TransactionRequestExt for TransactionRequest {
    fn with_max_fees(self, fees: MaxTxFeesPerGas) -> Self {
        self.with_max_fee_per_gas(fees.normal)
            .with_max_priority_fee_per_gas(fees.priority)
            .with_max_fee_per_blob_gas(fees.blob)
    }
}

pub fn at_horizon(mut value: u128, horizon: u32) -> u128 {
    for _ in 0..horizon {
        // multiply by 1.125 = multiply by 9, then divide by 8
        value = value.saturating_mul(9).saturating_div(8);
    }
    value
}

fn estimate_max_priority_fee_per_gas(fee_history: &FeeHistory) -> Result<u128> {
    // Taken from the default priority estimator for alloy
    let rewards = fee_history
        .reward
        .as_ref()
        .ok_or_else(|| Error::Other("reward not found in fee history".to_string()))?
        .iter()
        .filter_map(|r| r.first().copied())
        .filter(|r| *r > 0_u128)
        .sorted()
        .collect::<Vec<_>>();

    if rewards.is_empty() {
        return Ok(EIP1559_MIN_PRIORITY_FEE);
    }

    let n = rewards.len();

    let median = if n % 2 == 0 {
        (rewards[n / 2 - 1] + rewards[n / 2]) / 2
    } else {
        rewards[n / 2]
    };

    Ok(std::cmp::max(median, EIP1559_MIN_PRIORITY_FEE))
}
