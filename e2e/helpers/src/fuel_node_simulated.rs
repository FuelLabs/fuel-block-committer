use async_graphql::{Context, Object, Schema, SimpleObject};
use fuel_core_types::fuel_crypto::Hasher;
use hex;
use rand::rngs::SmallRng;
use rand::{RngCore, SeedableRng};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use tokio::sync::oneshot;
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

struct AppState {
    pub current_height: AtomicU32,
    pub block_contents: Vec<u8>,
}

impl AppState {
    pub fn latest_block(&self) -> Block {
        block_at_height(
            self.current_height
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    pub fn compressed_block(&self, _height: u32) -> DaCompressedBlock {
        let compressed = format!("0x{}", hex::encode(&self.block_contents));
        DaCompressedBlock {
            bytes: HexString(compressed),
        }
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

impl AppState {
    pub fn new(block_size: usize) -> Self {
        let mut rng = SmallRng::from_seed([0; 32]);
        let mut block_contents = vec![0; block_size];
        rng.fill_bytes(&mut block_contents);

        Self {
            current_height: AtomicU32::new(0),
            block_contents,
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

        height.map(|h| state.compressed_block(h.0))
    }
}

async fn produce_blocks(state: Arc<AppState>) {
    loop {
        sleep(Duration::from_secs(1)).await;
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
}

impl FuelNode {
    /// Creates a new instance with the specified port.
    pub fn new(port: u16) -> Self {
        Self {
            shutdown_tx: None,
            join_handle: None,
            port,
        }
    }

    pub fn url(&self) -> Url {
        Url::parse(&format!("http://localhost:{}/v1/graphql", self.port)).unwrap()
    }

    /// Runs the server. This spawns the GraphQL server and block producer in background tasks.
    pub fn run(&mut self, block_size: usize) {
        let state = Arc::new(AppState::new(block_size));
        let state_clone = state.clone();
        // Spawn the block production in a background task.
        tokio::spawn(async move {
            produce_blocks(state_clone).await;
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
