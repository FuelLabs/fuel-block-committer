use std::{ops::RangeInclusive, path::PathBuf};

use services::fees::{Fees, FeesAtHeight};
use tracing::{error, info};
use xdg::BaseDirectories;

use super::models::SavedFees;

pub const ETH_BLOCK_TIME: u64 = 12;
pub const FUEL_BLOCK_TIME: u64 = 1;

/// Path to the fee cache file.
pub fn fee_file() -> PathBuf {
    let xdg = BaseDirectories::with_prefix("fee_simulation").unwrap();
    if let Some(cache) = xdg.find_cache_file("fee_cache.json") {
        cache
    } else {
        xdg.place_data_file("fee_cache.json").unwrap()
    }
}

pub fn load_cache() -> Vec<(u64, Fees)> {
    let contents = match std::fs::read_to_string(fee_file()) {
        Ok(contents) => contents,
        Err(e) => {
            error!("Failed to read fee cache file: {e}");
            return vec![];
        }
    };

    let fees: SavedFees = serde_json::from_str(&contents)
        .inspect_err(|e| error!("error while deserializing json cache!: {e}"))
        .unwrap_or_default();

    info!("loaded from cache: {} fees", fees.fees.len());

    fees.fees.into_iter().map(|f| (f.height, f.fees)).collect()
}

pub fn save_cache(cache: impl IntoIterator<Item = (u64, Fees)>) -> anyhow::Result<()> {
    let fees = SavedFees {
        fees: cache
            .into_iter()
            .map(|(height, fees)| FeesAtHeight { height, fees })
            .collect(),
    };
    std::fs::write(fee_file(), serde_json::to_string(&fees)?)?;
    info!("saved to cache: {} fees", fees.fees.len());
    Ok(())
}

pub fn last_n_blocks(current_block: u64, n: std::num::NonZeroU64) -> RangeInclusive<u64> {
    current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
}

pub fn wei_to_eth_string(wei: u128) -> String {
    format!("{:.4}", (wei as f64) / 1e18)
}
