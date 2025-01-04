pub mod l1 {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Fees {
        pub base_fee_per_gas: u128,
        pub reward: u128,
        pub base_fee_per_blob_gas: u128,
    }

    impl Default for Fees {
        fn default() -> Self {
            Self {
                base_fee_per_gas: 1.try_into().unwrap(),
                reward: 1.try_into().unwrap(),
                base_fee_per_blob_gas: 1.try_into().unwrap(),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct BlockFees {
        pub height: u64,
        pub fees: Fees,
    }
    use std::ops::RangeInclusive;

    use itertools::Itertools;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Eq, Clone)]
    pub struct SequentialBlockFees {
        fees: Vec<BlockFees>,
    }

    // Doesn't detect that we use the contents in the Display impl
    #[allow(dead_code)]
    #[derive(Debug)]
    pub struct InvalidSequence(String);

    impl std::error::Error for InvalidSequence {}

    impl std::fmt::Display for InvalidSequence {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }

    impl IntoIterator for SequentialBlockFees {
        type Item = BlockFees;
        type IntoIter = std::vec::IntoIter<BlockFees>;
        fn into_iter(self) -> Self::IntoIter {
            self.fees.into_iter()
        }
    }

    impl FromIterator<BlockFees> for Result<SequentialBlockFees, InvalidSequence> {
        fn from_iter<T: IntoIterator<Item = BlockFees>>(iter: T) -> Self {
            SequentialBlockFees::try_from(iter.into_iter().collect::<Vec<_>>())
        }
    }

    // Cannot be empty
    #[allow(clippy::len_without_is_empty)]
    impl SequentialBlockFees {
        pub fn iter(&self) -> impl Iterator<Item = &BlockFees> {
            self.fees.iter()
        }

        pub fn last(&self) -> &BlockFees {
            self.fees.last().expect("not empty")
        }

        pub fn mean(&self) -> Fees {
            let count = self.len() as u128;

            let total = self
                .fees
                .iter()
                .map(|bf| bf.fees)
                .fold(Fees::default(), |acc, f| {
                    let base_fee_per_gas = acc.base_fee_per_gas.saturating_add(f.base_fee_per_gas);
                    let reward = acc.reward.saturating_add(f.reward);
                    let base_fee_per_blob_gas = acc
                        .base_fee_per_blob_gas
                        .saturating_add(f.base_fee_per_blob_gas);

                    Fees {
                        base_fee_per_gas,
                        reward,
                        base_fee_per_blob_gas,
                    }
                });

            Fees {
                base_fee_per_gas: total.base_fee_per_gas.saturating_div(count),
                reward: total.reward.saturating_div(count),
                base_fee_per_blob_gas: total.base_fee_per_blob_gas.saturating_div(count),
            }
        }

        pub fn len(&self) -> usize {
            self.fees.len()
        }

        pub fn height_range(&self) -> RangeInclusive<u64> {
            let start = self.fees.first().expect("not empty").height;
            let end = self.fees.last().expect("not empty").height;
            start..=end
        }
    }
    impl TryFrom<Vec<BlockFees>> for SequentialBlockFees {
        type Error = InvalidSequence;
        fn try_from(mut fees: Vec<BlockFees>) -> Result<Self, Self::Error> {
            if fees.is_empty() {
                return Err(InvalidSequence("Input cannot be empty".to_string()));
            }

            fees.sort_by_key(|f| f.height);

            let is_sequential = fees
                .iter()
                .tuple_windows()
                .all(|(l, r)| l.height + 1 == r.height);

            let heights = fees.iter().map(|f| f.height).collect::<Vec<_>>();
            if !is_sequential {
                return Err(InvalidSequence(format!(
                    "blocks are not sequential by height: {heights:?}"
                )));
            }

            Ok(Self { fees })
        }
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    #[cfg_attr(feature = "test-helpers", mockall::automock)]
    pub trait Api {
        async fn fees(
            &self,
            height_range: RangeInclusive<u64>,
        ) -> crate::Result<SequentialBlockFees>;
        async fn current_height(&self) -> crate::Result<u64>;
    }

    #[cfg(feature = "test-helpers")]
    pub mod testing {

        use std::{collections::BTreeMap, ops::RangeInclusive};

        use itertools::Itertools;

        use super::{Api, BlockFees, Fees, SequentialBlockFees};

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
            async fn fees(
                &self,
                height_range: RangeInclusive<u64>,
            ) -> crate::Result<SequentialBlockFees> {
                let fees = height_range
                    .into_iter()
                    .map(|height| BlockFees {
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

            async fn fees(
                &self,
                height_range: RangeInclusive<u64>,
            ) -> crate::Result<SequentialBlockFees> {
                let fees = self
                    .fees
                    .iter()
                    .skip_while(|(height, _)| !height_range.contains(height))
                    .take_while(|(height, _)| height_range.contains(height))
                    .map(|(height, fees)| BlockFees {
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
                    BlockFees {
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
    }
}
