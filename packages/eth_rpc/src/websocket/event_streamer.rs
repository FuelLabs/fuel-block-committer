use std::sync::Arc;

use ethers::{
    prelude::{k256::ecdsa::SigningKey, Event, SignerMiddleware},
    providers::{Provider, Ws},
    signers::Wallet,
};
use futures::{Stream, TryStreamExt};
use ports::types::FuelBlockCommittedOnL1;

use super::connection::CommitSubmittedFilter;
use crate::error::Result;

type EthStreamInitializer = Event<
    Arc<SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>>,
    SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>,
    CommitSubmittedFilter,
>;

pub struct EthEventStreamer {
    events: EthStreamInitializer,
}

impl EthEventStreamer {
    pub fn new(events: EthStreamInitializer) -> Self {
        Self { events }
    }

    pub(crate) async fn establish_stream(
        &self,
    ) -> Result<impl Stream<Item = Result<FuelBlockCommittedOnL1>> + Send + '_> {
        let events = self.events.subscribe().await?;
        let stream = events
            .map_ok(|event| {
                let fuel_block_hash = event.block_hash;
                let commit_height = event.commit_height;
                FuelBlockCommittedOnL1 {
                    fuel_block_hash,
                    commit_height,
                }
            })
            .map_err(Into::into);
        Ok(stream)
    }
}
