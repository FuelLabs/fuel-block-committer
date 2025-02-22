use async_graphql::{Context, InputValueError, Object, Schema, SchemaBuilder, SimpleObject, Value};
use fuel_core_types::fuel_crypto::Hasher;
use hex;
use std::sync::Arc;

// Import the AppState and types from the simulation module.
use super::simulation::AppState;

/// Custom scalar to represent a hex string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexString(pub String);

#[async_graphql::Scalar(name = "HexString")]
impl async_graphql::ScalarType for HexString {
    fn parse(value: Value) -> async_graphql::InputValueResult<Self> {
        match value {
            Value::String(s) => Ok(HexString(s)),
            _ => Err(InputValueError::expected_type(value)),
        }
    }

    fn to_value(&self) -> Value {
        Value::String(self.0.clone())
    }
}

/// Custom scalar to represent a U32 value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct U32(pub u32);

#[async_graphql::Scalar(name = "U32")]
impl async_graphql::ScalarType for U32 {
    fn parse(value: Value) -> async_graphql::InputValueResult<Self> {
        match value {
            Value::Number(n) => {
                if let Some(v) = n.as_u64() {
                    Ok(U32(v as u32))
                } else {
                    Err(InputValueError::custom("Invalid number"))
                }
            }
            Value::String(s) => s
                .parse::<u32>()
                .map(U32)
                .map_err(|_| InputValueError::custom("Invalid u32 string")),
            _ => Err(InputValueError::expected_type(value)),
        }
    }

    fn to_value(&self) -> Value {
        Value::String(self.0.to_string())
    }
}

/// GraphQL representation of a block.
#[derive(SimpleObject, Clone)]
#[graphql(name = "Block")]
pub struct Block {
    pub height: U32,
    pub id: String,
}

/// GraphQL representation of a compressed block.
#[derive(SimpleObject, Clone)]
#[graphql(name = "DaCompressedBlock")]
pub struct DaCompressedBlock {
    pub bytes: HexString,
}

/// GraphQL type that contains the chain information.
#[derive(SimpleObject)]
#[graphql(name = "ChainInfo")]
pub struct ChainInfo {
    pub latest_block: Block,
}

/// Query root for GraphQL.
pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// Returns the chain with the latest block.
    async fn chain(&self, ctx: &Context<'_>) -> ChainInfo {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        ChainInfo {
            latest_block: state.latest_block(),
        }
    }

    /// Returns a block at a given height.
    async fn block(&self, _ctx: &Context<'_>, height: Option<U32>) -> Option<Block> {
        height.map(|h| block_at_height(h.0))
    }

    /// Returns a compressed version of a block.
    async fn da_compressed_block(
        &self,
        ctx: &Context<'_>,
        height: Option<U32>,
    ) -> Option<DaCompressedBlock> {
        let state = ctx.data::<Arc<AppState>>().unwrap();
        Some(state.compressed_block(height.unwrap_or(U32(0)).0).await)
    }
}

/// Helper function to create a block at a given height.
pub fn block_at_height(height: u32) -> Block {
    Block {
        height: U32(height),
        id: id_for_height(height),
    }
}

/// Helper function to generate an id for a given block height.
pub fn id_for_height(height: u32) -> String {
    let mut hasher = Hasher::default();
    hasher.input(height.to_be_bytes());
    let digest = hasher.finalize();
    hex::encode(*digest)
}

/// Build and return the GraphQL schema.
pub fn build_schema(
) -> SchemaBuilder<QueryRoot, async_graphql::EmptyMutation, async_graphql::EmptySubscription> {
    Schema::build(
        QueryRoot,
        async_graphql::EmptyMutation,
        async_graphql::EmptySubscription,
    )
}
