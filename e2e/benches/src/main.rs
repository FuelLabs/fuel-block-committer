use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use e2e_helpers::{
    fuel_node_simulated::{Compressibility, FuelNode, SimulationConfig},
    whole_stack::{
        create_and_fund_kms_keys, deploy_contract, start_committer, start_db, start_eth, start_kms,
    },
};
use serde::Deserialize;
use tokio::sync::Mutex;
use warp::Filter;

// Structure to capture form submissions.
#[derive(Debug, Deserialize)]
struct ConfigForm {
    block_size: usize,
    compressibility: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize configuration with default values.
    let simulation_config = Arc::new(Mutex::new(SimulationConfig::new(
        128,
        Compressibility::Medium,
    )));

    // Start the simulated fuel node in a separate asynchronous task.
    let mut fuel_node = FuelNode::new(4000, simulation_config.clone());

    fuel_node.run().await;

    let logs = false;

    let kms = start_kms(logs).await?;

    let eth_node = start_eth(logs).await?;
    let (main_key, secondary_key) = create_and_fund_kms_keys(&kms, &eth_node).await?;

    let request_timeout = Duration::from_secs(50);
    let max_fee = 1_000_000_000_000;

    let (_contract_args, deployed_contract) =
        deploy_contract(&eth_node, &main_key, max_fee, request_timeout).await?;

    let db = start_db().await?;

    let _committer = start_committer(
        true,
        true,
        db.clone(),
        &eth_node,
        fuel_node.url(),
        &deployed_contract,
        &main_key,
        &secondary_key,
    )
    .await?;

    // Build the web server for the control panel.
    // We pass a clone of the shared configuration into the request filters.
    let config_filter = warp::any().map(move || simulation_config.clone());

    // GET / -> Serve the control panel page.
    let control_panel = warp::path::end()
        .and(warp::get())
        .and_then(serve_control_panel);

    // POST /update -> Accept updates from the form.
    let update_config = warp::path("update")
        .and(warp::post())
        .and(warp::body::form())
        .and(config_filter.clone())
        .and_then(handle_update);

    let routes = control_panel.or(update_config);

    println!("Control panel available at http://localhost:3030");
    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;

    Ok(())
}

// Serves a simple HTML control panel page using Bootstrap for styling.
async fn serve_control_panel() -> Result<impl warp::Reply, warp::Rejection> {
    let html = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Bench Control Panel</title>
    <link href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.0/dist/css/bootstrap.min.css" rel="stylesheet">
</head>
<body>
<div class="container">
    <h1 class="mt-5">Bench Control Panel</h1>
    <form action="/update" method="post" class="mt-3">
        <div class="mb-3">
            <label for="block_size" class="form-label">Block Size</label>
            <input type="number" class="form-control" id="block_size" name="block_size" value="128" required>
        </div>
        <div class="mb-3">
            <label for="compressibility" class="form-label">Compressibility</label>
            <select class="form-select" id="compressibility" name="compressibility">
                <option value="random">Random (No Compressibility)</option>
                <option value="low">Low</option>
                <option value="medium" selected>Medium</option>
                <option value="high">High</option>
                <option value="full">Full (Maximum Compressibility)</option>
            </select>
        </div>
        <button type="submit" class="btn btn-primary">Update Configuration</button>
    </form>
</div>
</body>
</html>"#;
    Ok(warp::reply::html(html))
}

// Handles form submission to update the simulation parameters.
async fn handle_update(
    form: ConfigForm,
    config: Arc<Mutex<SimulationConfig>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    println!("Received update: {:?}", form);
    let compressibility = match form.compressibility.parse::<Compressibility>() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error parsing compressibility: {}", e);
            Compressibility::Medium // fallback default
        }
    };
    {
        let mut cfg = config.lock().await;
        cfg.block_size = form.block_size;
        cfg.compressibility = compressibility;
        println!(
            "Updated config: block_size={}, compressibility={}",
            cfg.block_size, cfg.compressibility
        );
    }
    // Respond with a simple confirmation page.
    Ok(warp::reply::html("<html><body><div class='container mt-5'><p>Configuration updated successfully. <a href='/'>Go back</a></p></div></body></html>"))
}
