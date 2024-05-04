use std::sync::Arc;

use ethers::{
    prelude::{k256::ecdsa::SigningKey, Event, SignerMiddleware},
    providers::{Provider, Ws},
    signers::Wallet,
};
use futures::{Stream, TryStreamExt};
use ports::eth_rpc::FuelBlockCommittedOnEth;

use super::connection::CommitSubmittedFilter;
use crate::Result;

pub(crate) type EthStreamInitializer = Event<
    Arc<SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>>,
    SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>,
    CommitSubmittedFilter,
>;

pub struct EthEventStreamer {
    pub(crate) events: EthStreamInitializer,
}

impl EthEventStreamer {
    pub fn new(events: EthStreamInitializer) -> Self {
        Self { events }
    }

    pub(crate) async fn establish_stream(
        &self,
    ) -> Result<impl Stream<Item = Result<FuelBlockCommittedOnEth>> + Send + '_> {
        let events = self.events.subscribe().await?;
        let stream = events
            .map_ok(|event| {
                let fuel_block_hash = event.block_hash;
                let commit_height = event.commit_height;
                FuelBlockCommittedOnEth {
                    fuel_block_hash,
                    commit_height,
                }
            })
            .map_err(Into::into);
        Ok(stream)
    }
}
