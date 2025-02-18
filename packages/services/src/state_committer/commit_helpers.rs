use crate::state_committer::port::{fuel::Api as FuelApi, Storage};
use crate::types::{storage::BundleFragment, NonEmpty};
use crate::Result;
use metrics::prometheus::IntGauge;

pub async fn update_current_height_to_commit_metric<F, S>(
    fuel_api: &F,
    storage: &S,
    lookback_window: u32,
    gauge: &IntGauge,
) -> Result<()>
where
    F: FuelApi,
    S: Storage,
{
    let current_height_to_commit = if let Some(height) = storage.latest_bundled_height().await? {
        height.saturating_add(1)
    } else {
        fuel_api
            .latest_height()
            .await?
            .saturating_sub(lookback_window)
    };

    gauge.set(current_height_to_commit.into());
    Ok(())
}
