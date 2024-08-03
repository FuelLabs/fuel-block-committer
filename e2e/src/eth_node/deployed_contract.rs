use eth::WebsocketClient;
use ethers::{ abi::Address, types::Chain};
use ports::types::ValidatedFuelBlock;
use url::Url;

use crate::kms::KmsKey;
pub struct DeployedContract {
    address: Address,
    chain_state_contract: WebsocketClient,
}

impl DeployedContract {
    pub async fn connect(url: &Url, address: Address, key: KmsKey) -> anyhow::Result<Self> {
        let blob_wallet = None;
        let region: String = serde_json::to_string(&key.region)?;
        let chain_state_contract = WebsocketClient::connect(
            url,
            Chain::AnvilHardhat,
            address,
            key.id,
            blob_wallet,
            5,
            region,
            "test".to_string(),
            "test".to_string(),
            true,
        )
        .await?;

        Ok(Self {
            address,
            chain_state_contract,
        })
    }

    pub async fn finalized(&self, block: ValidatedFuelBlock) -> anyhow::Result<bool> {
        self.chain_state_contract
            .finalized(block)
            .await
            .map_err(Into::into)
    }

    pub fn address(&self) -> Address {
        self.address
    }
}
