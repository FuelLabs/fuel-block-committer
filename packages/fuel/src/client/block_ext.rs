use cynic::QueryBuilder;
use fuel_core_client::client::schema::da_compressed::DaCompressedBlock;
use fuel_core_client::client::schema::U32;
use fuel_core_client::client::{
    pagination::{PaginatedResult, PaginationRequest},
    schema::{
        block::{Consensus, Header},
        primitives::TransactionId,
        schema,
        tx::TransactionStatus,
        BlockId, ConnectionArgs, HexString, PageInfo,
    },
    FuelClient,
};
use ports::types::NonEmpty;

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    schema_path = "./target/schema.sdl",
    graphql_type = "Query",
    variables = "ConnectionArgs"
)]
pub struct FullBlocksQuery {
    #[arguments(after: $after, before: $before, first: $first, last: $last)]
    pub blocks: FullBlockConnection,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./target/schema.sdl", graphql_type = "BlockConnection")]
pub struct FullBlockConnection {
    pub edges: Vec<FullBlockEdge>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./target/schema.sdl", graphql_type = "BlockEdge")]
pub struct FullBlockEdge {
    pub cursor: String,
    pub node: FullBlock,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./target/schema.sdl", graphql_type = "Block")]
pub struct FullBlock {
    pub id: BlockId,
    pub header: Header,
    pub consensus: Consensus,
    pub transactions: Vec<OpaqueTransaction>,
}

#[derive(cynic::QueryVariables, Debug)]
pub struct DaCompressedBlockWithBlockIdByHeightArgs {
    height: U32,
    block_height: Option<U32>,
}

impl DaCompressedBlockWithBlockIdByHeightArgs {
    pub fn new(height: u32) -> Self {
        Self {
            height: height.into(),
            block_height: Some(height.into()),
        }
    }
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(
    schema_path = "./target/schema.sdl",
    graphql_type = "Query",
    variables = "DaCompressedBlockWithBlockIdByHeightArgs"
)]
pub struct DaCompressedBlockWithBlockIdByHeightQuery {
    #[arguments(height: $height)]
    pub da_compressed_block: Option<DaCompressedBlock>,
    #[arguments(height: $block_height)]
    pub block: Option<OnlyBlockId>,
}

pub struct DaCompressedBlockWithBlockId {
    pub da_compressed_block: DaCompressedBlock,
    pub block_id: BlockId,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./target/schema.sdl", graphql_type = "Block")]
pub struct OnlyBlockId {
    pub id: BlockId,
}

impl TryFrom<FullBlock> for ports::fuel::FullFuelBlock {
    type Error = crate::Error;

    fn try_from(value: FullBlock) -> Result<Self, Self::Error> {
        let raw_transactions = value
            .transactions
            .into_iter()
            .map(|t| {
                NonEmpty::collect(t.raw_payload.to_vec()).ok_or_else(|| {
                    crate::Error::Other(format!(
                        "encountered empty transaction in block: {}",
                        value.id
                    ))
                })
            })
            .collect::<Result<Vec<_>, Self::Error>>()?;

        let header = value.header.try_into().map_err(|e| {
            crate::Error::Other(format!(
                "failed to convert block header of fuel block {}: {e}",
                value.id
            ))
        })?;

        Ok(Self {
            id: value.id.into(),
            header,
            consensus: value.consensus.into(),
            raw_transactions,
        })
    }
}
impl From<FullBlockConnection> for PaginatedResult<FullBlock, String> {
    fn from(conn: FullBlockConnection) -> Self {
        PaginatedResult {
            cursor: conn.page_info.end_cursor,
            has_next_page: conn.page_info.has_next_page,
            has_previous_page: conn.page_info.has_previous_page,
            results: conn.edges.into_iter().map(|e| e.node).collect(),
        }
    }
}

#[derive(cynic::QueryFragment, Clone, Debug)]
#[cynic(schema_path = "./target/schema.sdl", graphql_type = "Transaction")]
pub struct OpaqueTransaction {
    pub id: TransactionId,
    pub raw_payload: HexString,
    pub status: Option<TransactionStatus>,
}

#[trait_variant::make(Send)]
pub trait ClientExt {
    async fn full_blocks(
        &self,
        request: PaginationRequest<String>,
    ) -> std::io::Result<PaginatedResult<FullBlock, String>>;

    async fn da_compressed_block_with_id(
        &self,
        height: u32,
    ) -> std::io::Result<Option<DaCompressedBlockWithBlockId>>;
}

impl ClientExt for FuelClient {
    async fn full_blocks(
        &self,
        request: PaginationRequest<String>,
    ) -> std::io::Result<PaginatedResult<FullBlock, String>> {
        let query = FullBlocksQuery::build(request.into());
        let blocks = self.query(query).await?.blocks.into();
        Ok(blocks)
    }

    async fn da_compressed_block_with_id(
        &self,
        height: u32,
    ) -> std::io::Result<Option<DaCompressedBlockWithBlockId>> {
        let query = DaCompressedBlockWithBlockIdByHeightQuery::build(
            DaCompressedBlockWithBlockIdByHeightArgs::new(height),
        );
        let da_compressed_block = self.query(query).await?;

        if let (Some(da_compressed), Some(block)) = (
            da_compressed_block.da_compressed_block,
            da_compressed_block.block,
        ) {
            Ok(Some(DaCompressedBlockWithBlockId {
                da_compressed_block: da_compressed,
                block_id: block.id,
            }))
        } else {
            Ok(None)
        }
    }
}
