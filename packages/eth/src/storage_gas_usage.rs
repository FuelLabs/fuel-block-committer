use std::num::NonZeroUsize;

use ports::l1::GasUsage;

use alloy::eips::eip4844::{
    DATA_GAS_PER_BLOB, FIELD_ELEMENTS_PER_BLOB, FIELD_ELEMENT_BYTES, MAX_BLOBS_PER_BLOCK,
    MAX_DATA_GAS_PER_BLOCK,
};

/// Intrinsic gas cost of a eth transaction.
const BASE_TX_COST: u64 = 21_000;

#[derive(Debug, Clone, Copy)]
pub struct Eip4844GasUsage;

impl ports::l1::StorageCostCalculator for Eip4844GasUsage {
    fn max_bytes_per_submission(&self) -> std::num::NonZeroUsize {
        ENCODABLE_BYTES_PER_TX.try_into().expect("always positive")
    }
    fn gas_usage_to_store_data(&self, num_bytes: NonZeroUsize) -> ports::l1::GasUsage {
        gas_usage_to_store_data(num_bytes)
    }
}

fn gas_usage_to_store_data(num_bytes: NonZeroUsize) -> GasUsage {
    let num_bytes =
        u64::try_from(num_bytes.get()).expect("to not have more than u64::MAX of storage data");

    // Taken from the SimpleCoder impl
    let required_fe = num_bytes.div_ceil(31).saturating_add(1);

    // alloy constants not used since they are u64
    let blob_num = required_fe.div_ceil(FIELD_ELEMENTS_PER_BLOB);

    const MAX_BLOBS_PER_BLOCK: u64 = MAX_DATA_GAS_PER_BLOCK / DATA_GAS_PER_BLOB;
    let number_of_txs = blob_num.div_ceil(MAX_BLOBS_PER_BLOCK);

    let storage = blob_num.saturating_mul(DATA_GAS_PER_BLOB);
    let normal = number_of_txs * BASE_TX_COST;

    GasUsage { storage, normal }
}

// 1 whole field element is lost plus a byte for every remaining field element
const ENCODABLE_BYTES_PER_TX: usize = (FIELD_ELEMENT_BYTES as usize - 1)
    * (FIELD_ELEMENTS_PER_BLOB as usize * MAX_BLOBS_PER_BLOCK - 1);

#[cfg(test)]
mod tests {
    use alloy::consensus::{SidecarBuilder, SimpleCoder};
    use rand::{rngs::SmallRng, Rng, SeedableRng};
    use test_case::test_case;

    use super::*;

    #[test_case(100, 1, 1; "single eth tx with one blob")]
    #[test_case(129 * 1024, 1, 2; "single eth tx with two blobs")]
    #[test_case(257 * 1024, 1, 3; "single eth tx with three blobs")]
    #[test_case(385 * 1024, 1, 4; "single eth tx with four blobs")]
    #[test_case(513 * 1024, 1, 5; "single eth tx with five blobs")]
    #[test_case(740 * 1024, 1, 6; "single eth tx with six blobs")]
    #[test_case(768 * 1024, 2, 7; "two eth tx with seven blobs")]
    #[test_case(896 * 1024, 2, 8; "two eth tx with eight blobs")]
    fn gas_usage_for_data_storage(num_bytes: usize, num_txs: usize, num_blobs: usize) {
        // given

        // when
        let usage = gas_usage_to_store_data(num_bytes.try_into().unwrap());

        // then
        assert_eq!(usage.normal as usize, num_txs * 21_000);
        assert_eq!(
            usage.storage as u64,
            num_blobs as u64 * alloy::eips::eip4844::DATA_GAS_PER_BLOB
        );

        let mut rng = SmallRng::from_seed([0; 32]);
        let mut data = vec![0; num_bytes];
        rng.fill(&mut data[..]);

        let mut builder = SidecarBuilder::from_coder_and_capacity(SimpleCoder::default(), 0);
        builder.ingest(&data);

        assert_eq!(builder.build().unwrap().blobs.len(), num_blobs,);
    }

    #[test]
    fn encodable_bytes_per_tx_correctly_calculated() {
        let mut rand_gen = SmallRng::from_seed([0; 32]);
        let mut max_bytes = [0; ENCODABLE_BYTES_PER_TX];
        rand_gen.fill(&mut max_bytes[..]);

        let mut builder = SidecarBuilder::from_coder_and_capacity(SimpleCoder::default(), 6);
        builder.ingest(&max_bytes);

        assert_eq!(builder.build().unwrap().blobs.len(), 6);

        let mut one_too_many = [0; ENCODABLE_BYTES_PER_TX + 1];
        rand_gen.fill(&mut one_too_many[..]);
        let mut builder = SidecarBuilder::from_coder_and_capacity(SimpleCoder::default(), 6);
        builder.ingest(&one_too_many);

        assert_eq!(builder.build().unwrap().blobs.len(), 7);
    }
}
