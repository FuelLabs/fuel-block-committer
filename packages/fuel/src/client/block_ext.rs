use cynic::QueryBuilder;
use fuel_core_client::client::{
    pagination::{PaginatedResult, PaginationRequest},
    schema::{
        block::{BlockByHeightArgs, Consensus, Header},
        primitives::TransactionId,
        schema,
        tx::TransactionStatus,
        BlockId, ConnectionArgs, HexString, PageInfo,
    },
    FuelClient,
};
use fuel_core_types::fuel_crypto::PublicKey;

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
#[cynic(
    schema_path = "./target/schema.sdl",
    graphql_type = "Query",
    variables = "BlockByHeightArgs"
)]
pub struct FullBlockByHeightQuery {
    #[arguments(height: $height)]
    pub block: Option<FullBlock>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(schema_path = "./target/schema.sdl", graphql_type = "Block")]
pub struct FullBlock {
    pub id: BlockId,
    pub header: Header,
    pub consensus: Consensus,
    pub transactions: Vec<OpaqueTransaction>,
}

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

//#[cfg(test)] // TODO: @hal3e check what to do with this test
//mod tests {
//    use super::*;
//    use fuel_core_client::client::pagination::PageDirection;
//
//    #[tokio::test]
//    async fn testnet_works() {
//        let client = FuelClient::new("https://testnet.fuel.network")
//            .expect("Should connect to the beta 5 network");
//
//        let request = PaginationRequest {
//            cursor: None,
//            results: 1,
//            direction: PageDirection::Backward,
//        };
//        let full_block = client.full_blocks(request).await;
//
//        assert!(full_block.is_ok(), "{full_block:?}");
//    }
//}
