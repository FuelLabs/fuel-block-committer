use super::models::SavedFees;
use anyhow::Result;
use services::historical_fees::port::cache::CachingApi;
use std::{ops::RangeInclusive, path::PathBuf};
use xdg::BaseDirectories;

/// Path to the fee cache file.
pub fn fee_file() -> PathBuf {
    let xdg = BaseDirectories::with_prefix("fee_simulation").unwrap();
    if let Some(cache) = xdg.find_cache_file("fee_cache.json") {
        cache
    } else {
        xdg.place_data_file("fee_cache.json").unwrap()
    }
}

/// Load fees from the cache file.
pub fn load_cache() -> Vec<(u64, services::historical_fees::port::l1::Fees)> {
    let Ok(contents) = std::fs::read_to_string(fee_file()) else {
        return vec![];
    };
    let fees: SavedFees = serde_json::from_str(&contents).unwrap_or_default();
    fees.fees.into_iter().map(|f| (f.height, f.fees)).collect()
}

/// Save fees to the cache file.
pub fn save_cache(
    cache: impl IntoIterator<Item = (u64, services::historical_fees::port::l1::Fees)>,
) -> anyhow::Result<()> {
    let fees = SavedFees {
        fees: cache
            .into_iter()
            .map(|(height, fees)| services::historical_fees::port::l1::BlockFees { height, fees })
            .collect(),
    };
    std::fs::write(fee_file(), serde_json::to_string(&fees)?)?;
    Ok(())
}

/// Helper to create and configure CachingApi.
pub struct CachingApiBuilder {
    client: eth::HttpClient,
    cache_size: usize,
}

impl CachingApiBuilder {
    pub fn new(client: eth::HttpClient, cache_size: usize) -> Self {
        Self { client, cache_size }
    }

    pub async fn build(self) -> Result<CachingApi<eth::HttpClient>> {
        let caching_api = CachingApi::new(self.client, self.cache_size);
        Ok(caching_api)
    }
}

/// Helper to calculate the last N blocks as a range.
pub fn last_n_blocks(current_block: u64, n: std::num::NonZeroU64) -> RangeInclusive<u64> {
    current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
}
