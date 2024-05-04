use futures::TryStreamExt;
use ports::eth_rpc::FuelBlockCommittedOnEth;

use futures::Stream;

use ethers::prelude::k256::ecdsa::SigningKey;

use ethers::signers::Wallet;

use ethers::providers::{Provider, Ws};

use ethers::prelude::SignerMiddleware;

use std::sync::Arc;

use ethers::prelude::Event;

use crate::Result;

use super::connection::CommitSubmittedFilter;

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
