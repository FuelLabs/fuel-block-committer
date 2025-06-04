use crate::Result;
use crate::state_committer::port::{Storage, fuel::Api as FuelApi};
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
