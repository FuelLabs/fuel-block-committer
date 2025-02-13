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

pub async fn next_fragments_to_submit<F, S>(
    fuel_api: &F,
    storage: &S,
    lookback_window: u32,
    max_fragments: usize,
) -> Result<Option<NonEmpty<BundleFragment>>>
where
    F: FuelApi,
    S: Storage,
{
    let latest_height = fuel_api.latest_height().await?;
    let starting_height = latest_height.saturating_sub(lookback_window);

    let existing_fragments = storage
        .oldest_nonfinalized_fragments(starting_height, max_fragments)
        .await?;
    let fragments = NonEmpty::collect(existing_fragments);

    Ok(fragments)
}
