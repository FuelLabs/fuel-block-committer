use std::{pin::Pin, sync::Arc};

use ethers::{
    prelude::{k256::ecdsa::SigningKey, Event, SignerMiddleware},
    providers::{Provider, Ws},
    signers::Wallet,
};
use fuels::tx::Bytes32;
use futures::{stream::TryStreamExt, Stream};

use super::{EventStreamer, FuelBlockCommitedOnEth};
use crate::{
    adapters::ethereum_adapter::ethereum_rpc::CommitSubmittedFilter,
    errors::{Error, Result},
};

type EthStreamInitializer = Event<
    Arc<SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>>,
    SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>,
    CommitSubmittedFilter,
>;

pub struct EthEventStreamer {
    events: EthStreamInitializer,
}

#[async_trait::async_trait]
impl EventStreamer for EthEventStreamer {
    async fn establish_stream(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<FuelBlockCommitedOnEth>> + '_ + Send>>> {
        Ok(Box::pin(
            self.events
                .subscribe()
                .await
                .map_err(|e| Error::NetworkError(e.to_string()))?
                .map_ok(|event| {
                    let fuel_block_hash = Bytes32::from(event.block_hash);
                    let commit_height = event.commit_height;
                    FuelBlockCommitedOnEth {
                        fuel_block_hash,
                        commit_height,
                    }
                })
                .map_err(|e| Error::Other(e.to_string())),
        ))
    }
}

impl EthEventStreamer {
    pub fn new(events: EthStreamInitializer) -> Self {
        Self { events }
    }
}
