use async_graphql::{Context, Object, Schema, SimpleObject};
use hex;
use rand::Rng;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use warp::{http::Response, Filter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexString(pub String);

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
pub struct U32(pub u32);

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

#[derive(SimpleObject, Clone)]
#[graphql(name = "Block")]
pub struct Block {
    pub height: U32,
    pub id: String,
}

impl From<InternalBlock> for Block {
    fn from(b: InternalBlock) -> Self {
        Block {
            height: U32(b.height),
            id: b.id,
        }
    }
}

#[derive(async_graphql::SimpleObject)]
#[graphql(name = "DaCompressedBlock")]
pub struct DaCompressedBlock {
    pub bytes: HexString,
}

#[derive(SimpleObject)]
#[graphql(name = "ChainInfo")]
pub struct ChainInfo {
    pub latest_block: Block,
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    // Returns chain { latestBlock { height, id } }
    async fn chain(&self, ctx: &Context<'_>) -> Option<ChainInfo> {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        let blocks = state.blocks.lock().await;
        blocks.last().cloned().map(|b| ChainInfo {
            latest_block: b.into(),
        })
    }

    // Returns block(height: $height) { height, id }
    async fn block(&self, ctx: &Context<'_>, height: Option<U32>) -> Option<Block> {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        let blocks = state.blocks.lock().await;
        if let Some(U32(h)) = height {
            blocks
                .iter()
                .find(|b| b.height == h)
                .cloned()
                .map(|b| b.into())
        } else {
            None
        }
    }

    // Returns daCompressedBlock(height: $height) { bytes }
    async fn da_compressed_block(
        &self,
        ctx: &Context<'_>,
        height: Option<U32>,
    ) -> Option<DaCompressedBlock> {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        let blocks = state.blocks.lock().await;
        if let Some(U32(h)) = height {
            if let Some(b) = blocks.iter().find(|b| b.height == h) {
                let compressed = format!("0x{}", hex::encode(&b.data));
                return Some(DaCompressedBlock {
                    bytes: HexString(compressed),
                });
            }
        }
        None
    }
}

async fn produce_blocks(state: Arc<AppState>) {
    loop {
        sleep(Duration::from_secs(1)).await;
        let mut blocks = state.blocks.lock().await;
        let new_height = blocks.len() as u32 + 1;
        let block_id = hex::encode(rand::thread_rng().gen::<[u8; 32]>());
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
// Run the GraphQL server with warp on /v1/graphql.
//
pub async fn run_server(block_size: usize, port: u16) {
    let state = Arc::new(AppState::new(block_size));
    let state_clone = state.clone();
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
                Schema<QueryRoot, async_graphql::EmptyMutation, async_graphql::EmptySubscription>,
                async_graphql::Request,
            )| async move {
                Ok::<_, std::convert::Infallible>(warp::reply::json(&schema.execute(request).await))
            },
        );

    // GraphQL Playground for GET requests at /v1/graphql
    let playground = warp::path!("v1" / "graphql").and(warp::get()).map(|| {
        Response::builder()
            .header("content-type", "text/html")
            .body(async_graphql::http::playground_source(
                async_graphql::http::GraphQLPlaygroundConfig::new("/v1/graphql"),
            ))
    });

    let routes = graphql_post.or(playground).with(warp::log("fake_l2_node"));

    println!(
        "GraphQL server running on http://localhost:{}/v1/graphql",
        port
    );
    warp::serve(routes).run(([0, 0, 0, 0], port)).await;
}
