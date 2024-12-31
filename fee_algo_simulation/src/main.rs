use std::net::SocketAddr;

use anyhow::Result;
use axum::{routing::get, Router};
use services::historical_fees::service::HistoricalFees;

mod handlers;
mod models;
mod state;
mod utils;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber for logging
    tracing_subscriber::fmt::init();

    // Initialize the HTTP client for Ethereum RPC
    let client = eth::HttpClient::new(models::URL).unwrap();

    // Calculate the number of blocks per month (~259200 blocks)
    let num_blocks_per_month = 30 * 24 * 3600 / 12; // 259200 blocks

    // Build the CachingApi and import any existing cache
    let caching_api = utils::CachingApiBuilder::new(client, num_blocks_per_month * 2)
        .build()
        .await?;
    caching_api.import(utils::load_cache()).await;

    // Build HistoricalFees service
    let historical_fees = HistoricalFees::new(caching_api.clone());

    // Bundle everything into shared application state
    let state = state::AppState {
        caching_api: caching_api.clone(),
        historical_fees,
        num_blocks_per_month: num_blocks_per_month as u64,
    };

    // Set up Axum router with routes and shared state
    let app = Router::new()
        .route("/", get(handlers::index_html))
        .route("/fees", get(handlers::get_fees))
        .with_state(state);

    // Define the server address
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Server listening on http://{}", addr);

    // run our app with hyper
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();

    axum::serve(listener, app).await.unwrap();
    // Save cache on shutdown
    utils::save_cache(caching_api.export().await)?;

    Ok(())
}
