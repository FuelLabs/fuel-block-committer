use std::{
    net::SocketAddr,
    num::{NonZeroU32, NonZeroU64},
    ops::RangeInclusive,
    path::PathBuf,
};

use anyhow::Result;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use services::{
    historical_fees::{
        port::{
            cache::CachingApi,
            l1::{Api, BlockFees, Fees},
        },
        service::{calculate_blob_tx_fee, HistoricalFees, SmaPeriods},
    },
    state_committer::{AlgoConfig, FeeThresholds, Percentage, SmaFeeAlgo},
};
use xdg::BaseDirectories;

#[derive(Debug, Serialize, Deserialize, Default)]
struct SavedFees {
    fees: Vec<BlockFees>,
}

const URL: &str = "https://eth.llamarpc.com";

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
    caching_api: CachingApi<eth::HttpClient>,
    historical_fees: HistoricalFees<CachingApi<eth::HttpClient>>,
    num_blocks_per_month: u64,
}

/// Query params for /fees
#[derive(Debug, Deserialize)]
struct FeeParams {
    ending_height: Option<u64>,
    amount_of_blocks: Option<u64>,

    // Fee Algo settings
    short: Option<u64>,
    long: Option<u64>,
    max_l2_blocks_behind: Option<u32>,
    start_discount_percentage: Option<f64>,
    end_premium_percentage: Option<f64>,
    always_acceptable_fee: Option<String>,

    // Number of blobs per transaction
    num_blobs: Option<u32>,

    // How many L2 blocks behind are we? If none is given, default 0
    num_l2_blocks_behind: Option<u32>,
}

/// Response struct for each fee data point
#[derive(Debug, Serialize)]
struct FeeDataPoint {
    #[serde(rename = "blockHeight")]
    block_height: u64,
    #[serde(rename = "currentFee")]
    current_fee: String, // Serialize u128 as String
    #[serde(rename = "shortFee")]
    short_fee: String, // Serialize u128 as String
    #[serde(rename = "longFee")]
    long_fee: String, // Serialize u128 as String
    acceptable: bool,
}

/// Statistics struct
#[derive(Debug, Serialize)]
struct FeeStats {
    #[serde(rename = "percentageAcceptable")]
    percentage_acceptable: f64, // Percentage of acceptable blocks
    #[serde(rename = "percentile95GapSize")]
    percentile_95_gap_size: u64, // 95th percentile of gap sizes in blocks
    #[serde(rename = "longestUnacceptableStreak")]
    longest_unacceptable_streak: u64, // Longest consecutive unacceptable blocks
}

/// Complete response struct
#[derive(Debug, Serialize)]
struct FeeResponse {
    data: Vec<FeeDataPoint>,
    stats: FeeStats,
}

/// GET /fees
///
/// Returns JSON: { data: [...], stats: {...} }
async fn get_fees(
    State(state): State<AppState>,
    Query(params): Query<FeeParams>,
) -> impl IntoResponse {
    // 1) Resolve user inputs or use defaults
    let ending_height = params.ending_height.unwrap_or(21_514_918);
    let amount_of_blocks = params
        .amount_of_blocks
        .unwrap_or(state.num_blocks_per_month);

    let short = params.short.unwrap_or(25); // default short
    let long = params.long.unwrap_or(300); // default long

    let max_l2 = params.max_l2_blocks_behind.unwrap_or(8 * 3600);
    let start_discount = params.start_discount_percentage.unwrap_or(0.10);
    let end_premium = params.end_premium_percentage.unwrap_or(0.20);

    let always_acceptable_fee = match params.always_acceptable_fee {
        Some(v) => match v.parse::<u128>() {
            Ok(val) => val,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    "Invalid always_acceptable_fee value",
                )
                    .into_response()
            }
        },
        None => 1_000_000_000_000_000,
    };

    let num_blobs = params.num_blobs.unwrap_or(6); // default to 6 blobs

    let num_l2_blocks_behind = params.num_l2_blocks_behind.unwrap_or(0);

    // 2) Build an SmaFeeAlgo config from user’s inputs
    let config = AlgoConfig {
        sma_periods: SmaPeriods {
            short: match NonZeroU64::new(short) {
                Some(nz) => nz,
                None => NonZeroU64::new(1).unwrap(),
            },
            long: match NonZeroU64::new(long) {
                Some(nz) => nz,
                None => NonZeroU64::new(1).unwrap(),
            },
        },
        fee_thresholds: FeeThresholds {
            max_l2_blocks_behind: match NonZeroU32::new(max_l2) {
                Some(nz) => nz,
                None => NonZeroU32::new(1).unwrap(),
            },
            start_discount_percentage: match Percentage::try_from(start_discount) {
                Ok(p) => p,
                Err(_) => Percentage::ZERO,
            },
            end_premium_percentage: match Percentage::try_from(end_premium) {
                Ok(p) => p,
                Err(_) => Percentage::ZERO,
            },
            always_acceptable_fee,
        },
    };

    let sma_algo = SmaFeeAlgo::new(state.historical_fees.clone(), config);

    // 3) Determine which blocks to fetch
    let start_height = ending_height.saturating_sub(amount_of_blocks);
    let range = start_height..=ending_height;

    // 4) Actually fetch from the caching API
    let fees_res = state.caching_api.fees(range).await;
    let Ok(seq_fees) = fees_res else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch fees").into_response();
    };

    // 5) Prepare data points
    let mut data = Vec::with_capacity(seq_fees.len());

    for block_fees in seq_fees.into_iter() {
        let block_height = block_fees.height;
        let current_fee = calculate_blob_tx_fee(num_blobs, &block_fees.fees);

        // Fetch the shortTerm + longTerm SMA at exactly this height
        let short_term_sma = match state
            .historical_fees
            .calculate_sma(last_n_blocks(block_height, config.sma_periods.short))
            .await
        {
            Ok(f) => f,
            Err(_) => Fees::default(),
        };

        let long_term_sma = match state
            .historical_fees
            .calculate_sma(last_n_blocks(block_height, config.sma_periods.long))
            .await
        {
            Ok(f) => f,
            Err(_) => Fees::default(),
        };

        let short_fee = calculate_blob_tx_fee(num_blobs, &short_term_sma);
        let long_fee = calculate_blob_tx_fee(num_blobs, &long_term_sma);

        let acceptable = match sma_algo
            .fees_acceptable(num_blobs, num_l2_blocks_behind, block_height)
            .await
        {
            Ok(decision) => decision,
            Err(_) => false, // or handle error differently
        };

        data.push(FeeDataPoint {
            block_height,
            current_fee: current_fee.to_string(),
            short_fee: short_fee.to_string(),
            long_fee: long_fee.to_string(),
            acceptable,
        });
    }

    // 6) Calculate statistics
    let total_blocks = data.len() as f64;
    let acceptable_blocks = data.iter().filter(|d| d.acceptable).count() as f64;
    let percentage_acceptable = if total_blocks > 0.0 {
        (acceptable_blocks / total_blocks) * 100.0
    } else {
        0.0
    };

    // 7) Calculate gap sizes (streaks of unacceptable blocks)
    let mut gap_sizes = Vec::new();
    let mut current_gap = 0;

    for d in &data {
        if !d.acceptable {
            current_gap += 1;
        } else if current_gap > 0 {
            gap_sizes.push(current_gap);
            current_gap = 0;
        }
    }

    // Push the last gap if it ends with an unacceptable streak
    if current_gap > 0 {
        gap_sizes.push(current_gap);
    }

    // 8) Calculate the 95th percentile of gap sizes
    let percentile_95_gap_size = if !gap_sizes.is_empty() {
        let mut sorted_gaps = gap_sizes.clone();
        sorted_gaps.sort_unstable();
        let index = ((sorted_gaps.len() as f64) * 0.95).ceil() as usize - 1;
        sorted_gaps[index.min(sorted_gaps.len() - 1)]
    } else {
        0
    };

    // 9) Find the longest unacceptable streak
    let longest_unacceptable_streak = gap_sizes.iter().cloned().max().unwrap_or(0);

    let stats = FeeStats {
        percentage_acceptable,
        percentile_95_gap_size,
        longest_unacceptable_streak,
    };

    let response = FeeResponse { data, stats };

    // Return as JSON
    Json(response).into_response()
}

/// Helper: compute [current_block - (n-1) .. current_block]
fn last_n_blocks(current_block: u64, n: NonZeroU64) -> RangeInclusive<u64> {
    current_block.saturating_sub(n.get().saturating_sub(1))..=current_block
}

/// The HTML page at GET /
async fn index_html() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="UTF-8" />
  <title>Fee Simulator - Multiple Series + Shading</title>
  <script src="https://cdn.plot.ly/plotly-latest.min.js"></script>
  <style>
    body {
      font-family: Arial, sans-serif;
      margin: 20px;
    }
    #chart {
      width: 100%;
      height: 600px;
    }
    .stats {
      margin-top: 20px;
    }
    .stats div {
      margin-bottom: 5px;
    }
    /* Responsive design */
    @media (max-width: 768px) {
      #chart {
        height: 400px;
      }
    }
  </style>
</head>
<body>
  <h1>Fee Simulator - Multiple Series + Shading</h1>

  <div style="margin-bottom: 1em;">
    <label for="endingHeight">Ending Height:</label>
    <input type="number" id="endingHeight" value="21514918" />

    <label for="amountOfBlocks">Block Range:</label>
    <input type="number" id="amountOfBlocks" value="300" />

    <br />

    <label for="short">Short SMA (blocks):</label>
    <input type="number" id="short" value="25" />
    <label for="long">Long SMA (blocks):</label>
    <input type="number" id="long" value="300" />
    <br />

    <label for="maxL2">max_l2_blocks_behind:</label>
    <input type="number" id="maxL2" value="28800" />  <!-- e.g. 8 hours worth -->

    <label for="startDiscount">start_discount_percentage:</label>
    <input type="number" step="0.01" id="startDiscount" value="0.10" />

    <label for="endPremium">end_premium_percentage:</label>
    <input type="number" step="0.01" id="endPremium" value="0.20" />
    <br />

    <label for="alwaysAcceptable">always_acceptable_fee:</label>
    <input type="number" id="alwaysAcceptable" value="1000000000000000" />

    <label for="numBlobs">Number of Blobs:</label>
    <input type="number" id="numBlobs" value="6" min="1" max="10" />
    <br />

    <label for="l2Behind">num_l2_blocks_behind:</label>
    <input type="number" id="l2Behind" value="0" />

    <br />

    <!-- Preset Buttons for Quick Selection -->
    <button onclick="setPreset(216000)">Last 1 Month (~216k blocks)</button>
    <button onclick="setPreset(50400)">Last 1 Week (~50.4k blocks)</button>
    <button onclick="setPreset(21600)">Last 3 Days (~21.6k blocks)</button>
    <button onclick="setPreset(7200)">Last 1 Day (~7.2k blocks)</button>
    <button onclick="setPreset(1500)">Last 5 Hours (~1.5k blocks)</button>
  </div>

  <div id="chart"></div>

  <div class="stats">
    <h2>Statistics</h2>
    <div><strong>Percentage of Acceptable Blocks:</strong> <span id="percentageAcceptable">0%</span></div>
    <div><strong>95th Percentile of Gap Sizes:</strong> <span id="percentile95GapSize">0 blocks</span></div>
    <div><strong>Longest Unacceptable Streak:</strong> <span id="longestUnacceptableStreak">0 blocks</span></div>
  </div>

  <script>
    function setPreset(numBlocks) {
      document.getElementById('amountOfBlocks').value = numBlocks;
      fetchAndPlot();
    }

    // Helper function to identify acceptable regions
    function getAcceptableRegions(data) {
      let regions = [];
      let start = null;

      for (let i = 0; i < data.length; i++) {
        if (data[i].acceptable) {
          if (start === null) {
            start = data[i].blockHeight;
          }
        } else {
          if (start !== null) {
            regions.push({ start: start, end: data[i - 1].blockHeight });
            start = null;
          }
        }
      }

      // Handle case where the last data point is acceptable
      if (start !== null) {
        regions.push({ start: start, end: data[data.length - 1].blockHeight });
      }

      return regions;
    }

    async function fetchAndPlot() {
      const endingHeight       = document.getElementById('endingHeight').value;
      const amountOfBlocks     = document.getElementById('amountOfBlocks').value;
      const shortSma           = document.getElementById('short').value;
      const longSma            = document.getElementById('long').value;
      const maxL2              = document.getElementById('maxL2').value;
      const startDiscount      = document.getElementById('startDiscount').value;
      const endPremium         = document.getElementById('endPremium').value;
      const alwaysAcceptable   = document.getElementById('alwaysAcceptable').value;
      const numBlobs           = document.getElementById('numBlobs').value;
      const numL2BlocksBehind  = document.getElementById('l2Behind').value;

      // Construct query string
      const qs = new URLSearchParams({
        ending_height: endingHeight,
        amount_of_blocks: amountOfBlocks,
        short: shortSma,
        long: longSma,
        max_l2_blocks_behind: maxL2,
        start_discount_percentage: startDiscount,
        end_premium_percentage: endPremium,
        always_acceptable_fee: alwaysAcceptable,
        num_blobs: numBlobs,
        num_l2_blocks_behind: numL2BlocksBehind,
      }).toString();

      const url = '/fees?' + qs;
      try {
        const resp = await fetch(url);
        if (!resp.ok) {
          throw new Error(`Error: ${resp.status}`);
        }
        const response = await resp.json();

        // Extract data and stats
        const data = response.data;
        const stats = response.stats;

        // Update statistics in the UI
        document.getElementById('percentageAcceptable').innerText = `${stats.percentageAcceptable.toFixed(2)}%`;
        document.getElementById('percentile95GapSize').innerText = `${stats.percentile95GapSize} blocks`;
        document.getElementById('longestUnacceptableStreak').innerText = `${stats.longestUnacceptableStreak} blocks`;

        // Prepare data for plotting
        const x = data.map(d => d.blockHeight);
        const currentFees = data.map(d => parseFloat(d.currentFee));
        const shortFees   = data.map(d => parseFloat(d.shortFee));
        const longFees    = data.map(d => parseFloat(d.longFee));

        // Identify acceptable regions
        const acceptableRegions = getAcceptableRegions(data);

        // Define Plotly shapes for shading
        const shapes = acceptableRegions.map(region => ({
          type: 'rect',
          xref: 'x',
          yref: 'paper',
          x0: region.start,
          y0: 0,
          x1: region.end,
          y1: 1,
          fillcolor: 'rgba(0, 255, 0, 0.2)',
          line: {
            width: 0,
          },
        }));

        const traceCurrent = {
          x, 
          y: currentFees,
          mode: 'lines',
          name: 'Current Fee',
          line: {color: 'blue'},
        };
        const traceShort = {
          x,
          y: shortFees,
          mode: 'lines',
          name: 'Short SMA Fee',
          line: {color: 'red'},
        };
        const traceLong = {
          x,
          y: longFees,
          mode: 'lines',
          name: 'Long SMA Fee',
          line: {color: 'green'},
        };

        const layout = {
          title: 'Fees vs. Block Height',
          xaxis: { title: 'Block Height' },
          yaxis: { title: 'Fee (wei)' },
          legend: { orientation: 'h', x: 0, y: 1.1 },
          shapes: shapes, // Add the shaded regions
        };

        Plotly.newPlot('chart', [traceCurrent, traceShort, traceLong], layout);
      } catch (err) {
        // Display error message in the stats section
        document.getElementById('percentageAcceptable').innerText = `Error: ${err.message}`;
        document.getElementById('percentile95GapSize').innerText = `-`;
        document.getElementById('longestUnacceptableStreak').innerText = `-`;
        // Optionally, clear the chart
        Plotly.purge('chart');
        console.error(err);
      }
    }

    // Immediately plot once on page load
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
    let client = eth::HttpClient::new(URL).unwrap();

    // 2) ~1 month = 259200 blocks (approx) if 12s per block
    let num_blocks_per_month = 30 * 24 * 3600 / 12; // 259200 blocks

    // 3) Build your CachingApi & import any existing cache
    let caching_api = CachingApi::new(client, num_blocks_per_month * 2);
    caching_api.import(load_cache()).await;

    // 4) Build your HistoricalFees
    let historical_fees = HistoricalFees::new(caching_api.clone());

    // 5) Bundle everything into state
    let state = AppState {
        caching_api: caching_api.clone(),
        historical_fees,
        num_blocks_per_month: num_blocks_per_month as u64,
    };

    // 6) Axum router: serve front-end + fees endpoint
    let app = Router::new()
        .route("/", get(index_html))
        .route("/fees", get(get_fees))
        .with_state(state);

    // 7) Run server
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Server listening on http://{}", addr);

    // run our app with hyper
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();

    axum::serve(listener, app).await.unwrap();
    save_cache(caching_api.export().await)?;

    Ok(())
}
