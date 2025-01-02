use services::fee_metrics_tracker::port::cache::CachingApi;

/// Shared state across routes.
#[derive(Clone)]
pub struct AppState {
    pub fee_api: CachingApi<eth::HttpClient>,
}
