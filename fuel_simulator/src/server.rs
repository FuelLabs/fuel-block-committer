use std::sync::Arc;
use tokio::sync::Mutex;
use async_graphql::{Schema, Object, Context, SimpleObject};
use warp::{Filter, http::Response};
use tokio::time::{sleep, Duration};
use hex;

//
// Internal types and shared state
//

#[derive(Clone)]
pub struct InternalBlock {
    pub height: u32,
    pub id: String,
    pub data: Vec<u8>,
}

pub struct AppState {
    pub blocks: Mutex<Vec<InternalBlock>>,
    pub block_size: usize,
}

impl AppState {
    pub fn new(block_size: usize) -> Self {
        Self {
            blocks: Mutex::new(Vec::new()),
            block_size,
        }
    }
}

//
// GraphQL types – note that the types below match the cynic codegen interface.
//

#[derive(SimpleObject, Clone)]
#[graphql(name = "Block")]
pub struct Block {
    pub height: String, // Representing U32 as a string
    pub id: String,     // Representing BlockId as a string
}

impl From<InternalBlock> for Block {
    fn from(b: InternalBlock) -> Self {
        Block {
            height: b.height.to_string(),
            id: b.id,
        }
    }
}

#[derive(SimpleObject)]
#[graphql(name = "DaCompressedBlock")]
pub struct DaCompressedBlock {
    pub bytes: String, // Representing HexString as a string
}

#[derive(SimpleObject)]
#[graphql(name = "ChainInfo")]
pub struct ChainInfo {
    pub latest_block: Block,
}

//
// GraphQL Query Root
//

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    // Returns chain { latestBlock { height, id } }
    async fn chain(&self, ctx: &Context<'_>) -> Option<ChainInfo> {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        let blocks = state.blocks.lock().await;
        blocks.last().cloned().map(|b| ChainInfo { latest_block: b.into() })
    }

    // Returns block(height: $height) { height, id }
    async fn block(&self, ctx: &Context<'_>, height: Option<u32>) -> Option<Block> {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        let blocks = state.blocks.lock().await;
        if let Some(h) = height {
            blocks.iter().find(|b| b.height == h).cloned().map(|b| b.into())
        } else {
            None
        }
    }

    // Returns daCompressedBlock(height: $height) { bytes }
    async fn da_compressed_block(&self, ctx: &Context<'_>, height: Option<u32>) -> Option<DaCompressedBlock> {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        let blocks = state.blocks.lock().await;
        if let Some(h) = height {
            if let Some(b) = blocks.iter().find(|b| b.height == h) {
                // Simulate compression by hex-encoding the block's data.
                let compressed = hex::encode(&b.data);
                return Some(DaCompressedBlock { bytes: compressed });
            }
        }
        None
    }
}

//
// Background block production – a new block is produced every second.
//
async fn produce_blocks(state: Arc<AppState>) {
    loop {
        sleep(Duration::from_secs(1)).await;
        let mut blocks = state.blocks.lock().await;
        let new_height = blocks.len() as u32 + 1;
        // For simplicity, the block id is "block-<height>"
        let block_id = format!("block-{}", new_height);
        // Create dummy data with the configured block_size.
        let data = vec![0u8; state.block_size];
        let new_block = InternalBlock {
            height: new_height,
            id: block_id,
            data,
        };
        blocks.push(new_block);
        println!("Produced block {}", new_height);
    }
}

//
// Run the GraphQL server with warp.
//
pub async fn run_server(block_size: usize, port: u16) {
    let state = Arc::new(AppState::new(block_size));
    // Spawn the background task to produce blocks every second.
    let state_clone = state.clone();
    tokio::spawn(async move {
        produce_blocks(state_clone).await;
    });

    let schema = Schema::build(QueryRoot, async_graphql::EmptyMutation, async_graphql::EmptySubscription)
        .data(state)
        .finish();

    // GraphQL endpoint filter for POST requests.
    let graphql_filter = async_graphql_warp::graphql(schema).and_then(
        |(schema, request): (Schema<QueryRoot, async_graphql::EmptyMutation, async_graphql::EmptySubscription>, async_graphql::Request)| async move {
            Ok::<_, std::convert::Infallible>(warp::reply::json(&schema.execute(request).await))
        }
    );

    // GraphQL Playground for GET requests at the root ("/")
    let playground = warp::path::end().and(warp::get()).map(|| {
        Response::builder()
            .header("content-type", "text/html")
            .body(async_graphql::http::playground_source(
                async_graphql::http::GraphQLPlaygroundConfig::new("/graphql"),
            ))
    });

    // Combine routes: Playground at "/" and GraphQL endpoint at "/graphql"
    let routes = playground.or(warp::path("graphql").and(graphql_filter));

    println!("GraphQL server running on http://localhost:{}/graphql", port);
    warp::serve(routes).run(([0, 0, 0, 0], port)).await;
}
