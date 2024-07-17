use ports::types::U256;

const BLOB_BASE_FEE_UPDATE_FRACTION: u64 = 3338477;
const GAS_PER_BLOB: u64 = 131_072;
const MIN_BASE_FEE_PER_BLOB_GAS: u64 = 1;

// Calculate blob fee based on the EIP-4844 specs
// https://eips.ethereum.org/EIPS/eip-4844
pub fn calculate_blob_fee(excess_blob_gas: U256, num_blobs: u64) -> U256 {
    get_total_blob_gas(num_blobs) * get_base_fee_per_blob_gas(excess_blob_gas)
}

fn get_total_blob_gas(num_blobs: u64) -> U256 {
    (GAS_PER_BLOB * num_blobs).into()
}

fn get_base_fee_per_blob_gas(excess_blob_gas: U256) -> U256 {
    fake_exponential(
        MIN_BASE_FEE_PER_BLOB_GAS.into(),
        excess_blob_gas,
        BLOB_BASE_FEE_UPDATE_FRACTION.into(),
    )
}

fn fake_exponential(factor: U256, numerator: U256, denominator: U256) -> U256 {
    assert!(!denominator.is_zero(), "attempt to divide by zero");

    let mut i = 1;
    let mut output = U256::zero();
    let mut numerator_accum = factor * denominator;
    while !numerator_accum.is_zero() {
        output += numerator_accum;
        numerator_accum = (numerator_accum * numerator) / (denominator * i);
        i += 1;
    }
    output / denominator
}
