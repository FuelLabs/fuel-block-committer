use super::HistoricalFees;
use services::historical_fees::port::cache::CachingApi;

/// Shared state across routes.
#[derive(Clone)]
pub struct AppState {
    pub caching_api: CachingApi<eth::HttpClient>,
    pub historical_fees: HistoricalFees<CachingApi<eth::HttpClient>>,
}
