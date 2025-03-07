use std::cmp::max;

use alloy::{
    network::{TransactionBuilder, TransactionBuilder4844},
    providers::utils::EIP1559_MIN_PRIORITY_FEE,
    rpc::types::{FeeHistory, TransactionRequest},
};
use itertools::Itertools;
use services::types::L1Tx;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxTxFeesPerGas {
    pub normal: u128,
    pub priority: u128,
    pub blob: u128,
}

impl<'a> From<&'a L1Tx> for MaxTxFeesPerGas {
    fn from(value: &'a L1Tx) -> Self {
        Self {
            normal: value.max_fee,
            priority: value.priority_fee,
            blob: value.blob_fee,
        }
    }
}

impl MaxTxFeesPerGas {
    /// Returns a new [`MaxTxFeesPerGas`] instance with its `normal` fee adjusted to ensure
    /// that it is at least as high as the transaction's `priority` fee.
    ///
    /// Under EIP1559, the following requirements must be met:
    /// - `max_fee_per_gas` must be at least as high as the block's base fee.
    /// - `max_fee_per_gas` must be at least as high as the transaction's `max_priority_fee_per_gas`.
    pub fn normalized(self) -> Self {
        Self {
            normal: std::cmp::max(self.normal, self.priority),
            priority: self.priority,
            blob: self.blob,
        }
    }

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

    /// Projects the current fee estimates forward to account for potential increases in the base fee
    /// over a future time horizon. This is achieved by iteratively applying Ethereum’s fee growth
    /// mechanism, which effectively multiplies a fee by about 1.125 per block. In this projection,
    /// the blob fee and the normal fee are each adjusted over their predetermined horizons, and the
    /// normal fee is then increased by adding the current priority fee as a buffer.
    ///
    /// EIP1559 requires only that:
    /// - `max_fee_per_gas` ≥ `base_fee`
    /// - `max_fee_per_gas` ≥ `max_priority_fee_per_gas`
    ///
    /// However, since the miner’s effective tip is computed as `max_fee_per_gas - base_fee`,
    /// a sudden rise in the base fee can reduce the tip even if the above conditions are met.
    /// By adding the priority fee to the projected normal fee, this method attempts to preserve
    /// the intended tip.
    pub fn projected(&self) -> Self {
        const BLOB_FEE_HORIZON: u32 = 5;
        const FEE_HORIZON: u32 = 6;

        Self {
            normal: at_horizon(self.normal, FEE_HORIZON).saturating_add(self.priority),
            blob: at_horizon(self.blob, BLOB_FEE_HORIZON),
            priority: self.priority,
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

fn at_horizon(mut value: u128, horizon: u32) -> u128 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correctly_reads_fees_from_l1_tx() {
        // given
        let normal = 100;
        let blob = 50;
        let priority = 25;
        let l1tx = gen_l1_tx(normal, blob, priority);

        // when
        let fees: MaxTxFeesPerGas = (&l1tx).into();

        // then
        assert_eq!(
            fees,
            MaxTxFeesPerGas {
                normal,
                priority,
                blob,
            }
        );
    }

    #[test]
    fn double_method_doubles_all_fees() {
        // given
        let fees = MaxTxFeesPerGas {
            normal: 100,
            priority: 50,
            blob: 25,
        };

        // when
        let doubled = fees.double();

        // then
        assert_eq!(
            doubled,
            MaxTxFeesPerGas {
                normal: 200,
                priority: 100,
                blob: 50,
            }
        );
    }

    #[test]
    fn retain_max_method_returns_max_of_each_fee() {
        // given
        let fees1 = MaxTxFeesPerGas {
            normal: 100,
            priority: 50,
            blob: 25,
        };
        let fees2 = MaxTxFeesPerGas {
            normal: 150,
            priority: 40,
            blob: 30,
        };

        // when
        let retained = fees1.retain_max(fees2);
        // normal: max(100, 150) = 150
        // priority: max(50, 40) = 50
        // blob: max(25, 30) = 30
        assert_eq!(
            retained,
            MaxTxFeesPerGas {
                normal: 150,
                priority: 50,
                blob: 30,
            },
        );
    }

    #[test]
    fn test_project_fees_at_horizon() {
        // given
        let fees = MaxTxFeesPerGas {
            normal: 100,
            priority: 20,
            blob: 50,
        };

        // when
        let projected = fees.projected();

        // then
        assert_eq!(
            projected,
            MaxTxFeesPerGas {
                normal: 219,
                blob: 87,
                ..fees
            }
        );
    }

    #[test]
    fn normalized_updates_normal_when_lower_than_priority() {
        // given
        let fees = MaxTxFeesPerGas {
            normal: 50,    // Lower than the priority fee.
            priority: 100, // Intended miner tip.
            blob: 30,
        };

        // when
        let normalized_fees = fees.normalized();

        // then
        assert_eq!(
            normalized_fees,
            MaxTxFeesPerGas {
                normal: 100,
                ..normalized_fees
            }
        );
    }

    #[test]
    fn normalized_keeps_normal_when_higher_than_priority() {
        // given
        let fees = MaxTxFeesPerGas {
            normal: 150, // Higher than the priority fee.
            priority: 100,
            blob: 30,
        };

        // when
        let normalized_fees = fees.normalized();

        // then
        assert_eq!(fees, normalized_fees);
    }

    fn gen_l1_tx(max_fee: u128, blob_fee: u128, priority_fee: u128) -> L1Tx {
        L1Tx {
            max_fee,
            blob_fee,
            priority_fee,
            id: None,
            hash: Default::default(),
            nonce: Default::default(),
            created_at: Default::default(),
            state: services::types::TransactionState::Pending,
        }
    }
}
