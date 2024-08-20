use alloy::providers::Provider;
use alloy::sol_types::SolEvent;
use futures::{Stream, StreamExt};
use ports::types::FuelBlockCommittedOnL1;

use crate::error::Result;

use super::connection::{AlloyWs, IFuelStateContract::CommitSubmitted};

pub struct EthEventStreamer {
    filter: alloy::rpc::types::Filter,
    provider: AlloyWs,
}

impl EthEventStreamer {
    pub fn new(filter: alloy::rpc::types::Filter, provider: AlloyWs) -> Self {
        Self { filter, provider }
    }

    pub(crate) async fn establish_stream(
        &self,
    ) -> Result<impl Stream<Item = Result<FuelBlockCommittedOnL1>> + Send + '_> {
        let sub = self.provider.subscribe_logs(&self.filter).await?;

        let stream = sub.into_stream().map(|log| {
            let CommitSubmitted {
                blockHash,
                commitHeight,
            } = CommitSubmitted::decode_log_data(log.data(), false)?;
            Ok(FuelBlockCommittedOnL1 {
                fuel_block_hash: blockHash.into(),
                commit_height: alloy::primitives::U256::from(commitHeight),
            })
        });

        Ok(stream)
    }
}
