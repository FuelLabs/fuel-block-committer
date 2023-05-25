use std::sync::Arc;

use actix_web::dev::Url;
use ethers::prelude::*;
use fuels::accounts::fuel_crypto::fuel_types::Bytes20;

use crate::{
    errors::{Error, Result},
    AppState,
};

#[derive(Debug, Clone)]
pub struct CommitListener {
    contract_address: Address,
    ethereum_rpc: Url, // websocket
    app_state: AppState,
}

// smart contract setup
abigen!(
    FUEL_STATE_CONTRACT,
    r#"[
        event CommitSubmitted(uint256 indexed commitHeight, bytes32 blockHash)        
    ]"#,
);

impl CommitListener {
    pub fn new(ethereum_rpc: Url, contract_address: Bytes20, app_state: AppState) -> Self {
        let contract_address = Address::from_slice(contract_address.as_ref());

        Self {
            contract_address,
            // todo: this should be turned into websocket url
            ethereum_rpc,
            app_state,
        }
    }

    pub async fn run(&self) -> Result<()> {
        // websocket setup
        let provider = Provider::<Ws>::connect(self.ethereum_rpc.uri().to_string())
            .await
            .map_err(|e| Error::NetworkError(e.to_string()))?;

        let client = Arc::new(provider);

        // contract setup
        let contract = FUEL_STATE_CONTRACT::new(self.contract_address, client.clone());

        // event listener setup
        let events = contract
            .event::<CommitSubmittedFilter>()
            .from_block(16232696);

        let mut stream = events
            .stream()
            .await
            .map_err(|e| Error::NetworkError(e.to_string()))?
            .take(1);

        while let Some(Ok(event)) = stream.next().await {
            let _height = event.commit_height;
            let _block_hash = event.block_hash;

            // todo: update the state
            match self.app_state.lock() {
                Ok(_state) => {}
                Err(e) => {
                    println!("Error: {:?}", e);
                }
            }
        }

        Ok(())
    }
}
