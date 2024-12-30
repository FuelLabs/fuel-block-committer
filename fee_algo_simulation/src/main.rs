use std::{net::SocketAddr, ops::RangeInclusive, path::PathBuf};

use anyhow::Result;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use xdg::BaseDirectories;

use services::{
    block_importer::port::fuel::__mock_MockApi_Api::__compressed_blocks_in_height_range,
    historical_fees::{
        self,
        port::{
            cache::CachingApi,
            l1::{Api, BlockFees, Fees, SequentialBlockFees},
        },
        service::{calculate_blob_tx_fee, HistoricalFees, SmaPeriods},
    },
    state_committer::{AlgoConfig, FeeThresholds},
};

// If you're using ethers-based client:
use services::historical_fees::port::l1 as eth;

#[derive(Debug, Serialize, Deserialize, Default)]
struct SavedFees {
    fees: Vec<BlockFees>,
}

const URL: &str = "https://eth.llamarpc.com";

pub struct PersistentApi<P> {
    provider: P,
}

/// Same fee_cache.json location logic
fn fee_file() -> PathBuf {
    let xdg = BaseDirectories::with_prefix("fee_simulation").unwrap();
    if let Some(cache) = xdg.find_cache_file("fee_cache.json") {
        cache
    } else {
        xdg.place_data_file("fee_cache.json").unwrap()
    }
}

/// Load from disk
fn load_cache() -> Vec<(u64, Fees)> {
    let Ok(contents) = std::fs::read_to_string(fee_file()) else {
        return vec![];
    };
    let fees: SavedFees = serde_json::from_str(&contents).unwrap_or_default();
    fees.fees.into_iter().map(|f| (f.height, f.fees)).collect()
}

/// Save to disk
fn save_cache(cache: impl IntoIterator<Item = (u64, Fees)>) -> anyhow::Result<()> {
    let fees = SavedFees {
        fees: cache
            .into_iter()
            .map(|(height, fees)| BlockFees { height, fees })
            .collect(),
    };
    std::fs::write(fee_file(), serde_json::to_string(&fees)?)?;
    Ok(())
}

/// Shared state across routes
#[derive(Clone)]
struct AppState {
    caching_api: CachingApi<::eth::HttpClient>,
    historical_fees: HistoricalFees<CachingApi<::eth::HttpClient>>,
    default_config: AlgoConfig,
    num_blocks_per_month: u64,
}

/// Query params for /fees
#[derive(Debug, Deserialize)]
struct FeeParams {
    ending_height: Option<u64>,
    amount_of_blocks: Option<u64>,
}

/// GET /fees
/// Returns an array of `(height, blobFee)`.
async fn get_fees(
    State(state): State<AppState>,
    Query(params): Query<FeeParams>,
) -> impl IntoResponse {
    let ending_height = params.ending_height.unwrap_or(21_514_918);
    let amount_of_blocks = params
        .amount_of_blocks
        .unwrap_or(state.num_blocks_per_month);

    let start_height = ending_height.saturating_sub(amount_of_blocks);
    let range = start_height..=ending_height;

    // Actually fetch from the caching API
    let fees_res = state.caching_api.fees(range).await;
    let Ok(seq_fees) = fees_res else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch fees").into_response();
    };

    // Convert to (blockHeight, blobFee)
    let data: Vec<(u64, u128)> = seq_fees
        .into_iter()
        .map(|block_fees| {
            let blob_fee = calculate_blob_tx_fee(6, &block_fees.fees);
            (block_fees.height, blob_fee)
        })
        .collect();

    Json(data).into_response()
}

/// The HTML page at GET /
async fn index_html() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="UTF-8" />
  <title>Fee Simulator</title>
  <!-- Load Plotly from a CDN -->
  <script src="https://cdn.plot.ly/plotly-latest.min.js"></script>
</head>
<body>
  <h1>Fee Simulator - Plot Blob Tx Fee Only</h1>
  <div style="margin-bottom: 1em;">
    <label for="endingHeight">Ending Height:</label>
    <input type="number" id="endingHeight" value="21514918" />

    <label for="amountOfBlocks">Block Range:</label>
    <input type="number" id="amountOfBlocks" value="2160" />

    <button onclick="fetchAndPlot()">Fetch Fees</button>
  </div>

  <div style="margin-bottom:1em;">
    <!-- Some preset buttons -->
    <button onclick="setPreset(7200)">Last 1 day (7200 blocks)</button>
    <button onclick="setPreset(50400)">Last 1 week (50.4k blocks)</button>
    <button onclick="setPreset(216000)">Last 1 month (216k blocks)</button>
  </div>

  <div id="chart" style="width:900px; height:600px;"></div>

  <script>
    function setPreset(numBlocks) {
      document.getElementById('amountOfBlocks').value = numBlocks;
      fetchAndPlot();
    }

    async function fetchAndPlot() {
      const endingHeight = document.getElementById('endingHeight').value;
      const amountOfBlocks = document.getElementById('amountOfBlocks').value;

      const query = `?ending_height=${endingHeight}&amount_of_blocks=${amountOfBlocks}`;

      try {
        const resp = await fetch('/fees' + query);
        if (!resp.ok) {
          throw new Error('Failed to fetch fees: ' + resp.status);
        }
        const data = await resp.json();

        // data is an array of [height, blobFee].
        const heights = data.map(item => item[0]);
        const blobFees = data.map(item => item[1]);

        // Single trace: just plot the blobFee
        const traceBlob = {
          x: heights,
          y: blobFees,
          mode: 'lines',
          name: 'Blob Tx Fee'
        };

        const layout = {
          title: 'Blob Tx Fee vs. Block Height',
          xaxis: { title: 'Block Height' },
          yaxis: { title: 'Blob Tx Fee (wei)' }
        };

        Plotly.newPlot('chart', [traceBlob], layout);
      } catch (err) {
        alert('Error: ' + err.message);
      }
    }

    // Auto-run on page load
    fetchAndPlot();
  </script>
</body>
</html>
"#,
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1) Create your ETH HTTP client
    let client = ::eth::HttpClient::new(URL).unwrap();

    // 2) ~1 month = 2160 blocks (approx) if 12s per block
    let num_blocks_per_month = 30 * 24 * 3600 / 12;

    // 3) Build your CachingApi & import any existing cache
    let caching_api = CachingApi::new(client, num_blocks_per_month * 2);
    caching_api.import(load_cache()).await;

    // 4) Build your HistoricalFees
    let historical_fees = HistoricalFees::new(caching_api.clone());

    // 5) Same default config you had
    let default_config = AlgoConfig {
        sma_periods: SmaPeriods {
            short: 25.try_into().unwrap(),
            long: 300.try_into().unwrap(),
        },
        fee_thresholds: FeeThresholds {
            max_l2_blocks_behind: (8 * 3600).try_into().unwrap(),
            start_discount_percentage: 0.10.try_into().unwrap(),
            end_premium_percentage: 0.20.try_into().unwrap(),
            always_acceptable_fee: 1000000000000000,
        },
    };

    // 6) Bundle everything into state
    let state = AppState {
        caching_api,
        historical_fees,
        default_config,
        num_blocks_per_month: num_blocks_per_month as u64,
    };

    // 7) Axum router: serve front-end + fees endpoint
    let app = Router::new()
        .route("/", get(index_html))
        .route("/fees", get(get_fees))
        .with_state(state);

    // 8) Run server
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Server listening on http://{}", addr);

    // run our app with hyper
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();

    axum::serve(listener, app).await.unwrap();

    Ok(())
}
