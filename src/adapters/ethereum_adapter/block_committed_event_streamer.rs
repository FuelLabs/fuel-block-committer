use std::sync::Arc;

use ethers::{
    prelude::{k256::ecdsa::SigningKey, Event, SignerMiddleware},
    providers::{Provider, Ws},
    signers::Wallet,
};
use fuels::tx::Bytes32;
use futures::{stream::TryStreamExt, Stream};

use crate::{
    adapters::ethereum_adapter::ethereum_rpc::CommitSubmittedFilter,
    errors::{Error, Result},
};

type CommitEventStreamer = Event<
    Arc<SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>>,
    SignerMiddleware<Provider<Ws>, Wallet<SigningKey>>,
    CommitSubmittedFilter,
>;
pub struct BlockCommittedEventStreamer {
    events: CommitEventStreamer,
}

impl BlockCommittedEventStreamer {
    pub fn new(events: CommitEventStreamer) -> Self {
        Self { events }
    }
    pub async fn stream(&self) -> impl Stream<Item = Result<Bytes32>> + '_ {
        self.events
            .stream()
            .await
            .unwrap()
            .map_ok(|event| Bytes32::from(event.block_hash))
            .map_err(|e| Error::NetworkError(e.to_string()))
    }
}
