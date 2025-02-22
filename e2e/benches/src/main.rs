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

// A custom error type for metrics proxy failures.
#[derive(Debug)]
struct MetricsProxyError(String);
impl warp::reject::Reject for MetricsProxyError {}

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
    let fuel_node_url = fuel_node.url();
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

    // --- Build the web server for the control panel ---

    // A filter that always supplies the simulation config.
    let config_filter = warp::any().map(move || simulation_config.clone());

    // GET / -> Serve the control panel page (with live graph preview).
    let control_panel = warp::path::end()
        .and(warp::get())
        .and(config_filter.clone())
        .and_then(serve_control_panel);

    // POST /update -> Accept updates from the form.
    let update_config = warp::path("update")
        .and(warp::post())
        .and(warp::body::form())
        .and(config_filter.clone())
        .and_then(handle_update);

    // Proxy /proxy/metrics to the committer metrics URL.
    let metrics_url = committer_metrics_url.clone();
    let metrics_proxy = warp::path!("proxy" / "metrics")
        .and(warp::get())
        .and_then(move || {
            let metrics_url = metrics_url.clone();
            async move { proxy_metrics(metrics_url.to_string()).await }
        });

    // Combine all routes.
    let routes = control_panel.or(update_config).or(metrics_proxy);

    println!("Control panel available at http://localhost:3030");
    warp::serve(routes).run(([0, 0, 0, 0], 3030)).await;

    Ok(())
}

/// Serves the control panel page with a prefilled form and a live chart.
/// A Chart.js–based graph (in a canvas element) displays live metrics fetched from /proxy/metrics.
async fn serve_control_panel(
    config: Arc<Mutex<SimulationConfig>>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let cfg = config.lock().await;
    let current_block_size = cfg.block_size;
    let current_compress = cfg.compressibility.to_string().to_lowercase();
    drop(cfg); // release lock early

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>Bench Control Panel</title>
  <link href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.0/dist/css/bootstrap.min.css" rel="stylesheet">
  <!-- Load Chart.js (pinned version for compatibility) -->
  <script src="https://cdn.jsdelivr.net/npm/chart.js@3.9.1/dist/chart.min.js"></script>
  <!-- Load Luxon -->
  <script src="https://cdn.jsdelivr.net/npm/luxon@3/build/global/luxon.min.js"></script>
  <!-- Load Chart.js adapter for Luxon -->
  <script src="https://cdn.jsdelivr.net/npm/chartjs-adapter-luxon@1.0.0"></script>
  <!-- Load chartjs-plugin-streaming for realtime charts -->
  <script src="https://cdn.jsdelivr.net/npm/chartjs-plugin-streaming@2.0.0"></script>
  <script>
    // Register the streaming plugin.
    Chart.register(ChartStreaming);
    console.log("ChartStreaming registered");
  </script>
</head>
<body>
  <div class="container">
    <h1 class="mt-5">Bench Control Panel</h1>
    <form id="updateForm" action="/update" method="post" class="mt-3">
      <div class="mb-3">
        <label for="block_size" class="form-label">Block Size</label>
        <input type="number" class="form-control" id="block_size" name="block_size" value="{current_block_size}" required>
      </div>
      <div class="mb-3">
        <label for="compressibility" class="form-label">Compressibility</label>
        <select class="form-select" id="compressibility" name="compressibility">
          <option value="random" {sel_random}>Random (No Compressibility)</option>
          <option value="low" {sel_low}>Low</option>
          <option value="medium" {sel_medium}>Medium</option>
          <option value="high" {sel_high}>High</option>
          <option value="full" {sel_full}>Full (Maximum Compressibility)</option>
        </select>
      </div>
      <button type="submit" class="btn btn-primary">Update Configuration</button>
      <span id="updateStatus" class="ms-3"></span>
    </form>
    <hr class="my-5">
    <h2>Live Metrics Preview</h2>
    <canvas id="metricsChart" width="800" height="400"></canvas>
  </div>
  <script>
    // Intercept the form submission and update configuration via AJAX.
    document.getElementById("updateForm").addEventListener("submit", function(e) {{
      e.preventDefault(); // Prevent the full page refresh.
      const form = e.target;
      const formData = new FormData(form);
      // Convert formData to URL encoded string.
      const data = new URLSearchParams();
      for (const pair of formData) {{
          data.append(pair[0], pair[1]);
      }}
      fetch(form.action, {{
        method: "POST",
        body: data,
        headers: {{
          "Content-Type": "application/x-www-form-urlencoded"
        }}
      }})
      .then(response => response.text())
      .then(result => {{
        document.getElementById("updateStatus").textContent = "Configuration updated successfully.";
        // Optionally, clear the status after a few seconds.
        setTimeout(() => {{
          document.getElementById("updateStatus").textContent = "";
        }}, 3000);
      }})
      .catch(err => {{
        console.error("Error updating configuration:", err);
        document.getElementById("updateStatus").textContent = "Error updating configuration.";
      }});
    }});

    // Initialize the Chart.js chart with streaming plugin enabled.
    document.addEventListener("DOMContentLoaded", function() {{
      const ctx = document.getElementById('metricsChart').getContext('2d');
      const chart = new Chart(ctx, {{
        type: 'line',
        data: {{
          datasets: [
            {{
              label: "l2 blocks behind",
              borderColor: "rgb(255, 99, 132)",
              data: [],
              fill: false,
            }}
          ]
        }},
        options: {{
          scales: {{
            x: {{
              type: "realtime",
              realtime: {{
                delay: 2000,
                refresh: 5000,
                duration: 20000,
                onRefresh: function(chart) {{
                  console.log("onRefresh triggered");
                  fetch("/proxy/metrics")
                    .then(response => response.text())
                    .then(text => {{
                      function parseMetric(metricName) {{
                        const regex = new RegExp("^" + metricName + "\\s+(\\S+)", "m");
                        const match = text.match(regex);
                        return match ? parseFloat(match[1]) : null;
                      }}
                      const fuel_height = parseMetric("fuel_height");
                      const current_height_to_commit = parseMetric("current_height_to_commit");
                      if (fuel_height !== null && current_height_to_commit !== null) {{
                        const expr = fuel_height - current_height_to_commit;
                        const now = Date.now();
                        chart.data.datasets[0].data.push({{x: now, y: expr}});
                      }}
                    }})
                    .catch(err => console.error("Error fetching metrics:", err));
                }}
              }}
            }},
            y: {{
              beginAtZero: true
            }}
          }},
          plugins: {{
            legend: {{
              display: true,
            }},
          }}
        }}
      }});
    }});
  </script>
</body>
</html>
"#,
        current_block_size = current_block_size,
        sel_random = if current_compress == "random" {
            "selected"
        } else {
            ""
        },
        sel_low = if current_compress == "low" {
            "selected"
        } else {
            ""
        },
        sel_medium = if current_compress == "medium" {
            "selected"
        } else {
            ""
        },
        sel_high = if current_compress == "high" {
            "selected"
        } else {
            ""
        },
        sel_full = if current_compress == "full" {
            "selected"
        } else {
            ""
        },
    );
    Ok(warp::reply::html(html))
}

/// Handles form submission to update the simulation parameters.
/// After updating, it sends a 303 See Other redirect back to the control panel.
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
    // Build a 303 See Other redirect response.
    let redirect = warp::http::Response::builder()
        .status(warp::http::StatusCode::SEE_OTHER)
        .header("location", "/")
        .body("")
        .unwrap();
    Ok(redirect)
}

/// Proxies a GET request for /proxy/metrics to the committer metrics URL.
async fn proxy_metrics(metrics_url: String) -> Result<impl warp::Reply, warp::Rejection> {
    match reqwest::get(&metrics_url).await {
        Ok(resp) => match resp.text().await {
            Ok(body) => Ok(warp::reply::with_header(body, "Content-Type", "text/plain")),
            Err(e) => {
                eprintln!("Error reading metrics response: {}", e);
                Err(warp::reject::custom(MetricsProxyError(e.to_string())))
            }
        },
        Err(e) => {
            eprintln!("Error fetching metrics: {}", e);
            Err(warp::reject::custom(MetricsProxyError(e.to_string())))
        }
    }
}
