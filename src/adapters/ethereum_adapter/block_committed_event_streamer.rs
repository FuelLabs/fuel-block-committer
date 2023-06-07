use std::{pin::Pin, sync::Arc};

use ethers::{
    prelude::{k256::ecdsa::SigningKey, Event, SignerMiddleware},
    providers::{Provider, Ws},
    signers::Wallet,
};
use fuels::tx::Bytes32;
use futures::{stream::TryStreamExt, Stream};

use super::BlockCommittedEventStreamer;
use crate::{
    adapters::ethereum_adapter::ethereum_rpc::CommitSubmittedFilter,
    errors::{Error, Result},
};

type CommitEventStreamer = Event<
    Arc<SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>>,
    SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>,
    CommitSubmittedFilter,
>;

pub struct EthBlockCommittedEventStreamer {
    events: CommitEventStreamer,
}

#[async_trait::async_trait]
impl BlockCommittedEventStreamer for EthBlockCommittedEventStreamer {
    async fn establish_stream(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes32>> + '_ + Send>>> {
        Ok(Box::pin(
            self.events
                .stream()
                .await
                .map_err(|e| Error::NetworkError(e.to_string()))?
                .map_ok(|event| Bytes32::from(event.block_hash))
                .map_err(|e| Error::Other(e.to_string())),
        ))
    }
}

impl EthBlockCommittedEventStreamer {
    pub fn new(events: CommitEventStreamer) -> Self {
        Self { events }
    }
}
