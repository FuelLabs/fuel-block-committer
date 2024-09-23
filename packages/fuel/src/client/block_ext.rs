use cynic::QueryBuilder;
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
use fuel_core_types::fuel_crypto::PublicKey;
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

// impl TryFrom<FullBlock> for ports::fuel::FullFuelBlock {
//     type Error = crate::Error;
//
//     fn try_from(value: FullBlock) -> Result<Self, Self::Error> {
//         todo!()
//     }
// }

impl FullBlock {
    /// Returns the block producer public key, if any.
    pub fn block_producer(&self) -> Option<PublicKey> {
        let message = self.header.id.clone().into_message();
        match &self.consensus {
            Consensus::Genesis(_) => Some(Default::default()),
            Consensus::PoAConsensus(poa) => {
                let signature = poa.signature.clone().into_signature();
                let producer_pub_key = signature.recover(&message);
                producer_pub_key.ok()
            }
            Consensus::Unknown => None,
        }
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
}
