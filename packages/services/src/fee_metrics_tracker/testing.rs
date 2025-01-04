use std::{collections::BTreeMap, ops::RangeInclusive};

use itertools::Itertools;

use super::port::l1::{Api, Fees, FeesAtHeight, SequentialBlockFees};

#[derive(Debug, Clone, Copy)]
pub struct ConstantFeeApi {
    fees: Fees,
}

impl ConstantFeeApi {
    pub const fn new(fees: Fees) -> Self {
        Self { fees }
    }
}

impl Api for ConstantFeeApi {
    async fn fees(&self, height_range: RangeInclusive<u64>) -> crate::Result<SequentialBlockFees> {
        let fees = height_range
            .into_iter()
            .map(|height| FeesAtHeight {
                height,
                fees: self.fees,
            })
            .collect_vec();

        Ok(fees.try_into().unwrap())
    }

    async fn current_height(&self) -> crate::Result<u64> {
        Ok(0)
    }
}

#[derive(Debug, Clone)]
pub struct PreconfiguredFeeApi {
    fees: BTreeMap<u64, Fees>,
}

impl Api for PreconfiguredFeeApi {
    async fn current_height(&self) -> crate::Result<u64> {
        Ok(*self
            .fees
            .keys()
            .last()
            .expect("no fees registered with PreconfiguredFeesProvider"))
    }

    async fn fees(&self, height_range: RangeInclusive<u64>) -> crate::Result<SequentialBlockFees> {
        let fees = self
            .fees
            .iter()
            .skip_while(|(height, _)| !height_range.contains(height))
            .take_while(|(height, _)| height_range.contains(height))
            .map(|(height, fees)| FeesAtHeight {
                height: *height,
                fees: *fees,
            })
            .collect_vec();

        Ok(fees.try_into().expect("block fees not sequential"))
    }
}

impl PreconfiguredFeeApi {
    pub fn new(blocks: impl IntoIterator<Item = (u64, Fees)>) -> Self {
        Self {
            fees: blocks.into_iter().collect(),
        }
    }
}

pub fn incrementing_fees(num_blocks: u64) -> SequentialBlockFees {
    let fees = (0..num_blocks)
        .map(|i| {
            let fee = u128::from(i) + 1;
            FeesAtHeight {
                height: i,
                fees: Fees {
                    base_fee_per_gas: fee,
                    reward: fee,
                    base_fee_per_blob_gas: fee,
                },
            }
        })
        .collect::<Result<_, _>>();

    fees.unwrap()
}
