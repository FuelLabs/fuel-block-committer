use std::{str::FromStr, sync::Arc};

use ethers::{
    prelude::{abigen, SignerMiddleware},
    providers::{Provider, Ws},
    signers::{LocalWallet, Signer},
    types::{Chain, H160},
};
use fuels::tx::Bytes32;

pub struct FuelStateContract {
    provider: Provider<Ws>,
    contract: FUEL_STATE_CONTRACT<SignerMiddleware<Provider<Ws>, LocalWallet>>,
}

abigen!(
    FUEL_STATE_CONTRACT,
    r#"[
        function finalized(bytes32 blockHash, uint256 blockHeight) external view whenNotPaused returns (bool)
        function blockHashAtCommit(uint256 commitHeight) external view returns (bytes32)
    ]"#,
);

impl FuelStateContract {
    pub async fn connect(eth_node_port: u16) -> Self {
        let contract_address = "0xdAad669b06d79Cb48C8cfef789972436dBe6F24d";
        let provider = Provider::<Ws>::connect(format!("ws://127.0.0.1:{eth_node_port}"))
            .await
            .unwrap();

        let wallet = LocalWallet::from_str(
            "0xd7cb3084b252751f5a6a3ec06a267451d390724fdb3f572560d998af8d00dae0",
        )
        .unwrap()
        .with_chain_id(Chain::AnvilHardhat);

        let signer = SignerMiddleware::new(provider.clone(), wallet);

        let contract_address: H160 = contract_address.parse().unwrap();
        let contract = FUEL_STATE_CONTRACT::new(contract_address, Arc::new(signer));

        Self { provider, contract }
    }

    pub async fn finalized(&self, block_hash: Bytes32, block_height: u32) -> bool {
        self.contract
            .finalized(block_hash.into(), block_height.into())
            .call()
            .await
            .unwrap()
    }
    pub async fn block_hash_at_commit_height(&self, commit_height: u32) -> Bytes32 {
        self.contract
            .block_hash_at_commit(commit_height.into())
            .call()
            .await
            .unwrap()
            .into()
    }
}
