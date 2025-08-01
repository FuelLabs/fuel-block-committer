use std::sync::Arc;
use std::time::Duration;

use actix_web::{App, HttpResponse, HttpServer, web};
use anyhow::Result;
use e2e_helpers::{
    fuel_node_simulated::{Compressibility, FuelNode, SimulationConfig},
    whole_stack::{
        create_and_fund_kms_signers, deploy_contract, start_db, start_eigen_committer, start_eth,
        start_kms,
    },
};
use serde::Deserialize;
use tokio::sync::Mutex;

mod data;
mod handlers;
mod template;

#[actix_web::main]
async fn main() -> Result<()> {
    let simulation_config = Arc::new(Mutex::new(SimulationConfig::new(
        150_000,
        Compressibility::Medium,
    )));

    let mut fuel_node = FuelNode::new(4000, simulation_config.clone());
    fuel_node.run().await?;

    let logs = false;
    let kms = start_kms(logs).await?;
    let eth_node = start_eth(logs).await?;
    let eth_signers = create_and_fund_kms_signers(&kms, &eth_node).await?;
    let eigen_key = std::env::var("EIGEN_KEY").expect("EIGEN_KEY environment variable must be set");
    let max_fee = 1_000_000_000_000;
    let (_contract_args, deployed_contract) =
        deploy_contract(&eth_node, eth_signers.clone(), max_fee, request_timeout).await?;
    let db = start_db().await?;

    let logs = true;
    let committer = start_eigen_committer(
        logs,
        db.clone(),
        &eth_node,
        &fuel_node.url(),
        &deployed_contract,
        eth_signers.main,
        eigen_key,
        "28 MB",
    )
    .await?;

    let app_data = web::Data::new(data::AppData {
        simulation_config: simulation_config.clone(),
        metrics_url: committer.metrics_url().to_string(),
    });

    println!("Control panel available at http://localhost:3030");

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
