use std::{str::FromStr, sync::Arc, time::Duration};

use async_trait::async_trait;
use ethers::{
    prelude::{abigen, SignerMiddleware},
    providers::{Http, Middleware, Provider},
    signers::LocalWallet,
    types::{Address, H256},
};
use fuels::accounts::fuel_crypto::fuel_types::Bytes20;
use fuels::types::block::Block;
use tracing::{info, log::warn};
use url::Url;

use crate::errors::{Error, Result};

#[async_trait]
pub trait TxSubmitter {
    async fn submit(&self, block: Block) -> Result<H256>; //TODO: change to eth tx_id type
}

pub struct FakeTxSubmitter {}
#[async_trait]
impl TxSubmitter for FakeTxSubmitter {
    async fn submit(&self, _block: Block) -> Result<H256> {
        Ok(H256::default())
    }
}

abigen!(
    FUEL_STATE_CONTRACT,
    r#"[
        function commit(bytes32 blockHash, uint256 commitHeight) external whenNotPaused
    ]"#,
);

pub struct EthTxSubmitter {
    contract: FUEL_STATE_CONTRACT<SignerMiddleware<Provider<Http>, LocalWallet>>,
    provider: Provider<Http>,
}

#[async_trait]
impl TxSubmitter for EthTxSubmitter {
    async fn submit(&self, block: Block) -> Result<H256> {
        let contract_call = self.contract.commit(*block.id, block.header.height.into());
        let tx = contract_call
            .send()
            .await
            .map_err(|e| Error::NetworkError(e.to_string()));
        let id = tx?.tx_hash();

        warn!("{}", &id);

        for _ in 0..2 {
            let receipt = self
                .provider
                .get_transaction_receipt(id)
                .await
                .expect("failed to get transaction receipt");

            info!("{receipt:?}");

            tokio::time::sleep(Duration::from_secs(4)).await;
        }

        Ok(id)
    }
}

impl EthTxSubmitter {
    pub fn new(etherem_rpc: Url, contract_address: Bytes20, wallet: LocalWallet) -> Self {
        let provider = Provider::<Http>::try_from(etherem_rpc.to_string()).unwrap();
        let signer = SignerMiddleware::new(provider.clone(), wallet);

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FUEL_STATE_CONTRACT::new(contract_address, Arc::new(signer));

        Self {
            contract,
            provider: provider.clone(),
        }
    }
}
