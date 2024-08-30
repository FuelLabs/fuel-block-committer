use alloy::{primitives::U256, providers::Provider, rpc::types::Filter, sol_types::SolEvent};
use futures::{Stream, StreamExt};
use ports::types::FuelBlockCommittedOnL1;

use super::connection::{IFuelStateContract::CommitSubmitted, WsProvider};
use crate::error::Result;

pub struct EthEventStreamer {
    filter: Filter,
    provider: WsProvider,
}

impl EthEventStreamer {
    pub fn new(filter: Filter, provider: WsProvider) -> Self {
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
                commit_height: U256::from(commitHeight),
            })
        });

        Ok(stream)
    }
}
