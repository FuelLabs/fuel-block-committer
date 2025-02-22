use async_graphql::{Context, Object, Schema, SimpleObject};
use fuel_core_types::fuel_crypto::Hasher;
use hex;
use rand::rngs::SmallRng;
use rand::{Rng, RngCore, SeedableRng};
use std::str::FromStr;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use tokio::time::{sleep, Duration};
use url::Url;
use warp::{http::Response, Filter};

#[derive(Debug, Clone, PartialEq, Eq)]
struct HexString(pub String);

#[async_graphql::Scalar(name = "HexString")]
impl async_graphql::ScalarType for HexString {
    fn parse(value: async_graphql::Value) -> async_graphql::InputValueResult<Self> {
        match value {
            async_graphql::Value::String(s) => Ok(HexString(s)),
            _ => Err(async_graphql::InputValueError::expected_type(value)),
        }
    }

    fn to_value(&self) -> async_graphql::Value {
        async_graphql::Value::String(self.0.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct U32(pub u32);

#[async_graphql::Scalar(name = "U32")]
impl async_graphql::ScalarType for U32 {
    fn parse(value: async_graphql::Value) -> async_graphql::InputValueResult<Self> {
        match value {
            async_graphql::Value::Number(n) => {
                if let Some(v) = n.as_u64() {
                    Ok(U32(v as u32))
                } else {
                    Err(async_graphql::InputValueError::custom("Invalid number"))
                }
            }
            async_graphql::Value::String(s) => s
                .parse::<u32>()
                .map(U32)
                .map_err(|_| async_graphql::InputValueError::custom("Invalid u32 string")),
            _ => Err(async_graphql::InputValueError::expected_type(value)),
        }
    }

    fn to_value(&self) -> async_graphql::Value {
        async_graphql::Value::String(self.0.to_string())
    }
}

/// Application state now holds the block contents in an async mutex.
pub struct AppState {
    pub current_height: AtomicU32,
    pub block_contents: Mutex<Vec<u8>>,
}

impl AppState {
    /// Returns the latest block (based solely on height).
    pub fn latest_block(&self) -> Block {
        block_at_height(
            self.current_height
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Returns a compressed representation of the current block.
    /// (In this example, we simply hex-encode the raw block data.)
    pub async fn compressed_block(&self, _height: u32) -> DaCompressedBlock {
        let block_contents = self.block_contents.lock().await;
        let compressed = format!("0x{}", hex::encode(&*block_contents));
        DaCompressedBlock {
            bytes: HexString(compressed),
        }
    }

    /// Creates a new AppState with an initial block generated from the given block size.
    pub fn new(block_size: usize) -> Self {
        let mut rng = SmallRng::from_seed([0; 32]);
        let mut block = vec![0; block_size];
        rng.fill_bytes(&mut block);
        Self {
            current_height: AtomicU32::new(0),
            block_contents: Mutex::new(block),
        }
    }
}

#[derive(SimpleObject, Clone)]
#[graphql(name = "Block")]
struct Block {
    pub height: U32,
    pub id: String,
}

#[derive(async_graphql::SimpleObject)]
#[graphql(name = "DaCompressedBlock")]
struct DaCompressedBlock {
    pub bytes: HexString,
}

#[derive(SimpleObject)]
#[graphql(name = "ChainInfo")]
struct ChainInfo {
    pub latest_block: Block,
}

struct QueryRoot;

#[Object]
impl QueryRoot {
    // Returns chain { latestBlock { height, id } }
    async fn chain(&self, ctx: &Context<'_>) -> ChainInfo {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        ChainInfo {
            latest_block: state.latest_block(),
        }
    }

    // Returns block(height: $height) { height, id }
    async fn block(&self, _ctx: &Context<'_>, height: Option<U32>) -> Option<Block> {
        height.map(|h| block_at_height(h.0))
    }

    // Returns daCompressedBlock(height: $height) { bytes }
    async fn da_compressed_block(
        &self,
        ctx: &Context<'_>,
        height: Option<U32>,
    ) -> Option<DaCompressedBlock> {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        // For simplicity, we ignore the provided height.
        Some(state.compressed_block(height.unwrap_or(U32(0)).0).await)
    }
}

fn block_at_height(height: u32) -> Block {
    Block {
        height: U32(height),
        id: id_for_height(height),
    }
}

fn id_for_height(height: u32) -> String {
    let mut hasher = Hasher::default();
    hasher.input(height.to_be_bytes());
    let digest = hasher.finalize();
    hex::encode(*digest)
}

/// Helper function to generate block contents based on the given configuration.
/// For "Random", all bytes are random. For "Full", all bytes are zero.
/// For intermediate levels, a certain percentage of the bytes are zeroed.
fn generate_block_contents(block_size: usize, compressibility: &Compressibility) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let mut block = vec![0u8; block_size];
    match compressibility {
        Compressibility::Random => {
            rng.fill_bytes(&mut block);
        }
        Compressibility::Full => {
            // Leave all as zero.
        }
        Compressibility::Low => {
            rng.fill_bytes(&mut block);
            // Approximately 25% zeros.
            for byte in block.iter_mut() {
                if rng.gen_bool(0.25) {
                    *byte = 0;
                }
            }
        }
        Compressibility::Medium => {
            rng.fill_bytes(&mut block);
            // Approximately 50% zeros.
            for byte in block.iter_mut() {
                if rng.gen_bool(0.50) {
                    *byte = 0;
                }
            }
        }
        Compressibility::High => {
            rng.fill_bytes(&mut block);
            // Approximately 75% zeros.
            for byte in block.iter_mut() {
                if rng.gen_bool(0.75) {
                    *byte = 0;
                }
            }
        }
    }
    block
}

/// The block production loop now re-reads the simulation configuration at every iteration
/// and regenerates the block contents accordingly.
async fn produce_blocks(state: Arc<AppState>, config: Arc<Mutex<SimulationConfig>>) {
    loop {
        sleep(Duration::from_secs(1)).await;

        // Obtain current simulation parameters.
        let current_config = {
            let cfg = config.lock().await;
            (cfg.block_size, cfg.compressibility.clone())
        };

        // Generate new block contents based on the current config.
        let new_block = generate_block_contents(current_config.0, &current_config.1);
        {
            let mut block_contents = state.block_contents.lock().await;
            *block_contents = new_block;
        }

        state
            .current_height
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A RAII wrapper for the GraphQL server.
pub struct FuelNode {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<tokio::task::JoinHandle<()>>,
    port: u16,
    config: Arc<Mutex<SimulationConfig>>,
}

/// Shared simulation configuration.
#[derive(Clone)]
pub struct SimulationConfig {
    pub block_size: usize,
    pub compressibility: Compressibility,
}

/// Define compressibility options.
#[derive(Debug, Clone)]
pub enum Compressibility {
    Random,
    Low,
    Medium,
    High,
    Full,
}

impl FromStr for Compressibility {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "random" => Ok(Compressibility::Random),
            "low" => Ok(Compressibility::Low),
            "medium" => Ok(Compressibility::Medium),
            "high" => Ok(Compressibility::High),
            "full" => Ok(Compressibility::Full),
            _ => Err(format!("Invalid compressibility option: {}", s)),
        }
    }
}

impl std::fmt::Display for Compressibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Compressibility::Random => "Random",
            Compressibility::Low => "Low",
            Compressibility::Medium => "Medium",
            Compressibility::High => "High",
            Compressibility::Full => "Full",
        };
        write!(f, "{}", label)
    }
}

impl SimulationConfig {
    pub fn new(block_size: usize, compressibility: Compressibility) -> Self {
        Self {
            block_size,
            compressibility,
        }
    }
}

impl FuelNode {
    /// Creates a new instance with the specified port.
    pub fn new(port: u16, config: Arc<Mutex<SimulationConfig>>) -> Self {
        Self {
            shutdown_tx: None,
            join_handle: None,
            port,
            config,
        }
    }

    pub fn url(&self) -> Url {
        Url::parse(&format!("http://localhost:{}/v1/graphql", self.port)).unwrap()
    }

    /// Runs the server. This spawns the GraphQL server and the block producer in background tasks.
    pub async fn run(&mut self) {
        // Lock the configuration to get the initial block size.
        let initial_block_size = { self.config.lock().await.block_size };
        let state = Arc::new(AppState::new(initial_block_size));
        let state_clone = state.clone();
        let config_clone = self.config.clone();

        // Spawn the block production loop.
        tokio::spawn(async move {
            produce_blocks(state_clone, config_clone).await;
        });

        let schema = Schema::build(
            QueryRoot,
            async_graphql::EmptyMutation,
            async_graphql::EmptySubscription,
        )
        .data(state)
        .finish();

        let graphql_post = warp::path!("v1" / "graphql")
            .and(warp::post())
            .and(async_graphql_warp::graphql(schema.clone()))
            .and_then(
                |(schema, request): (
                    Schema<
                        QueryRoot,
                        async_graphql::EmptyMutation,
                        async_graphql::EmptySubscription,
                    >,
                    async_graphql::Request,
                )| async move {
                    Ok::<_, std::convert::Infallible>(warp::reply::json(
                        &schema.execute(request).await,
                    ))
                },
            );

        // GraphQL Playground for GET requests at /v1/graphql.
        let playground = warp::path!("v1" / "graphql").and(warp::get()).map(|| {
            Response::builder()
                .header("content-type", "text/html")
                .body(async_graphql::http::playground_source(
                    async_graphql::http::GraphQLPlaygroundConfig::new("/v1/graphql"),
                ))
        });

        let routes = graphql_post.or(playground).with(warp::log("fake_l2_node"));

        // Create a oneshot channel for graceful shutdown.
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        self.shutdown_tx = Some(shutdown_tx);

        let port = self.port;
        eprintln!(
            "GraphQL server running on http://localhost:{}/v1/graphql",
            port
        );

        let (_, fut) =
            warp::serve(routes).bind_with_graceful_shutdown(([0, 0, 0, 0], port), async {
                // Wait for the shutdown signal.
                let _ = shutdown_rx.await;
            });

        // Spawn the server using `bind_with_graceful_shutdown`.
        let join_handle = tokio::spawn(fut);
        self.join_handle = Some(join_handle);
    }

    /// Signals the server to shut down and waits for it to finish.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for FuelNode {
    fn drop(&mut self) {
        // If the server is still running, try to signal shutdown.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Abort the task if it has not finished.
        if let Some(handle) = self.join_handle.take() {
            handle.abort();
        }
    }
}
