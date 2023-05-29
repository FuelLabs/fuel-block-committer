use std::sync::Arc;
use tracing::info;

use crate::errors::{Error, Result};
use ethers::prelude::*;
use fuels::accounts::fuel_crypto::fuel_types::Bytes20;
use url::Url;

#[derive(Debug, Clone)]
pub struct CommitListener {
    _contract_address: Address,
    _ethereum_rpc: Url, // websocket
}

// smart contract setup
abigen!(
    FUEL_STATE_CONTRACT,
    r#"[
        event CommitSubmitted(uint256 indexed commitHeight, bytes32 blockHash)
    ]"#,
);

impl CommitListener {
    pub fn _new(_ethereum_rpc: Url, _contract_address: Bytes20) -> Self {
        let _contract_address = Address::from_slice(_contract_address.as_ref());

        Self {
            _contract_address,
            // todo: this should be turned into websocket url
            _ethereum_rpc,
        }
    }

    pub async fn _run(&self) -> Result<()> {
        // websocket setup
        let provider = Provider::<Ws>::connect(self._ethereum_rpc.to_string())
            .await
            .map_err(|e| Error::NetworkError(e.to_string()))?;
        let client = Arc::new(provider);

        // contract setup
        let contract = FUEL_STATE_CONTRACT::new(self._contract_address, client.clone());

        // event listener setup
        let events = contract.event::<CommitSubmittedFilter>().from_block(1);

        let mut stream = events
            .stream()
            .await
            .map_err(|e| Error::NetworkError(e.to_string()))?;

        loop {
            if let Some(Ok(event)) = stream.next().await {
                let _height = event.commit_height;

                info!("{_height}");
            }
        }
    }
}
