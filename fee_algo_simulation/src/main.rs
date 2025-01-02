use std::net::SocketAddr;

use actix_web::web::{self, Data};
use anyhow::Result;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

mod handlers;
mod models;
mod state;
mod utils;

#[tokio::main]
async fn main() -> Result<()> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env()?
        .add_directive("services::state_committer::fee_algo=off".parse()?);

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .compact()
        .init();

    // Initialize the HTTP client for Ethereum RPC
    let client = eth::HttpClient::new(models::URL).unwrap();

    // Calculate the number of blocks per month (~259200 blocks)
    let num_blocks_per_month = 30 * 24 * 3600 / 12; // 259200 blocks

    // Build the CachingApi and import any existing cache
    let caching_api = utils::CachingApiBuilder::new(client, num_blocks_per_month * 2)
        .build()
        .await?;
    caching_api.import(utils::load_cache()).await;

    // Bundle everything into shared application state
    let state = state::AppState {
        fee_api: caching_api.clone(),
    };

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let server = actix_web::HttpServer::new(move || {
        actix_web::App::new()
            .app_data(Data::new(state.clone()))
            .service(web::resource("/").route(web::get().to(handlers::index_html)))
            .service(web::resource("/fees").route(web::get().to(handlers::get_fees)))
    })
    .bind(addr)?;

    // Define the server address
    eprintln!("Server listening on http://{}", addr);

    server.run().await?;

    // Save cache on shutdown
    utils::save_cache(caching_api.export().await)?;

    Ok(())
}
