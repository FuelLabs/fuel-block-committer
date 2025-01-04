use std::{ops::RangeInclusive, path::PathBuf};

use tracing::{error, info};
use xdg::BaseDirectories;

use super::models::SavedFees;

/// Path to the fee cache file.
pub fn fee_file() -> PathBuf {
    let xdg = BaseDirectories::with_prefix("fee_simulation").unwrap();
    if let Some(cache) = xdg.find_cache_file("fee_cache.json") {
        cache
    } else {
        xdg.place_data_file("fee_cache.json").unwrap()
    }
}

pub fn load_cache() -> Vec<(u64, services::fee_metrics_tracker::port::l1::Fees)> {
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

pub fn save_cache(
    cache: impl IntoIterator<Item = (u64, services::fee_metrics_tracker::port::l1::Fees)>,
) -> anyhow::Result<()> {
    let fees = SavedFees {
        fees: cache
            .into_iter()
            .map(
                |(height, fees)| services::fee_metrics_tracker::port::l1::FeesAtHeight {
                    height,
                    fees,
                },
            )
            .collect(),
    };
    std::fs::write(fee_file(), serde_json::to_string(&fees)?)?;
    info!("saved to cache: {} fees", fees.fees.len());
    Ok(())
}

pub fn last_n_blocks(current_block: u64, n: std::num::NonZeroU64) -> RangeInclusive<u64> {
    current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
}
