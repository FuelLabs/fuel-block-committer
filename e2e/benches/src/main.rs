use std::sync::Arc;
use std::time::Duration;

use actix_web::{web, App, HttpResponse, HttpServer};
use anyhow::Result;
use e2e_helpers::{
    fuel_node_simulated::{Compressibility, FuelNode, SimulationConfig},
    whole_stack::{
        create_and_fund_kms_keys, deploy_contract, start_committer, start_db, start_eth, start_kms,
    },
};
use serde::Deserialize;
use tokio::sync::Mutex;

mod data;
mod handlers;
mod template;

#[actix_web::main]
async fn main() -> Result<()> {
    // Initialize configuration with default values.
    let simulation_config = Arc::new(Mutex::new(SimulationConfig::new(
        150_000,
        Compressibility::Medium,
    )));

    // Start the simulated fuel node in a separate asynchronous task.
    let mut fuel_node = FuelNode::new(4000, simulation_config.clone());
    let fuel_node_url = fuel_node.url();
    fuel_node.run().await?;

    let logs = false;
    let kms = start_kms(logs).await?;
    let eth_node = start_eth(logs).await?;
    let (main_key, secondary_key) = create_and_fund_kms_keys(&kms, &eth_node).await?;
    let request_timeout = Duration::from_secs(50);
    let max_fee = 1_000_000_000_000;
    let (_contract_args, deployed_contract) =
        deploy_contract(&eth_node, &main_key, max_fee, request_timeout).await?;
    let db = start_db().await?;
    let committer = start_committer(
        true,
        true,
        db.clone(),
        &eth_node,
        &fuel_node_url,
        &deployed_contract,
        &main_key,
        &secondary_key,
    )
    .await?;

    // Get the committer's metrics URL.
    let committer_metrics_url = committer.metrics_url();

    // Create shared AppData.
    let app_data = web::Data::new(data::AppData {
        simulation_config: simulation_config.clone(),
        metrics_url: committer_metrics_url.to_string(),
    });

    println!("Control panel available at http://localhost:3030");

    // Build and run the Actix‑Web server.
    HttpServer::new(move || {
        App::new()
            .app_data(app_data.clone())
            .route("/", web::get().to(handlers::serve_control_panel))
            .route("/update", web::post().to(handlers::update_config))
            .route("/proxy/metrics", web::get().to(handlers::proxy_metrics))
    })
    .bind("0.0.0.0:3030")?
    .run()
    .await?;

    Ok(())
}
