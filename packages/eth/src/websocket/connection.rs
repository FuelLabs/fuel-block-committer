use std::num::NonZeroU32;

use alloy::{
    consensus::{SidecarBuilder, SimpleCoder},
    network::{Ethereum, EthereumWallet, TransactionBuilder, TxSigner},
    primitives::{Address, U256},
    providers::{
        fillers::{ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller, WalletFiller},
        Identity, Provider, ProviderBuilder, RootProvider, WsConnect,
    },
    pubsub::PubSubFrontend,
    rpc::types::{TransactionReceipt, TransactionRequest},
    signers::aws::AwsSigner,
    sol,
};
use ports::types::{NonEmptyVec, TransactionResponse, ValidatedFuelBlock};
use url::Url;

use super::{event_streamer::EthEventStreamer, health_tracking_middleware::EthApi};
use crate::error::{Error, Result};

pub type WsProvider = FillProvider<
    JoinFill<
        JoinFill<JoinFill<JoinFill<Identity, GasFiller>, NonceFiller>, ChainIdFiller>,
        WalletFiller<EthereumWallet>,
    >,
    RootProvider<PubSubFrontend>,
    PubSubFrontend,
    Ethereum,
>;

type FuelStateContract = IFuelStateContract::IFuelStateContractInstance<
    PubSubFrontend,
    FillProvider<
        JoinFill<
            JoinFill<JoinFill<JoinFill<Identity, GasFiller>, NonceFiller>, ChainIdFiller>,
            WalletFiller<EthereumWallet>,
        >,
        RootProvider<PubSubFrontend>,
        PubSubFrontend,
        Ethereum,
    >,
>;

sol!(
    #[sol(rpc)]
    interface IFuelStateContract {
        function commit(bytes32 blockHash, uint256 commitHeight) external whenNotPaused;
        event CommitSubmitted(uint256 indexed commitHeight, bytes32 blockHash);
        function finalized(bytes32 blockHash, uint256 blockHeight) external view whenNotPaused returns (bool);
        function blockHashAtCommit(uint256 commitHeight) external view returns (bytes32);
        function BLOCKS_PER_COMMIT_INTERVAL() external view returns (uint256);
    }
);

#[derive(Clone)]
pub struct WsConnection {
    provider: WsProvider,
    blob_provider: Option<WsProvider>,
    address: Address,
    blob_signer_address: Option<Address>,
    contract: FuelStateContract,
    commit_interval: NonZeroU32,
}

#[async_trait::async_trait]
impl EthApi for WsConnection {
    fn split_into_submittable_state_chunks(
        &self,
        data: &[u8],
    ) -> Result<NonEmptyVec<NonEmptyVec<u8>>> {
        todo!()
    }

    async fn submit(&self, block: ValidatedFuelBlock) -> Result<()> {
        let commit_height = Self::calculate_commit_height(block.height(), self.commit_interval);
        let contract_call = self.contract.commit(block.hash().into(), commit_height);
        let tx = contract_call.send().await?;
        tracing::info!("tx: {} submitted", tx.tx_hash());

        Ok(())
    }

    async fn get_block_number(&self) -> Result<u64> {
        let response = self.provider.get_block_number().await?;
        Ok(response)
    }

    async fn balance(&self) -> Result<U256> {
        let address = self.address;
        Ok(self.provider.get_balance(address).await?)
    }

    fn commit_interval(&self) -> NonZeroU32 {
        self.commit_interval
    }

    fn event_streamer(&self, eth_block_height: u64) -> EthEventStreamer {
        let filter = self
            .contract
            .CommitSubmitted_filter()
            .from_block(eth_block_height)
            .filter;
        EthEventStreamer::new(filter, self.contract.provider().clone())
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<Option<TransactionResponse>> {
        let tx_receipt = self
            .provider
            .get_transaction_receipt(tx_hash.into())
            .await?;

        Self::convert_to_tx_response(tx_receipt)
    }

    async fn submit_l2_state(&self, state_data: Vec<u8>) -> Result<[u8; 32]> {
        let (blob_provider, blob_signer_address) =
            match (&self.blob_provider, &self.blob_signer_address) {
                (Some(provider), Some(address)) => (provider, address),
                _ => return Err(Error::Other("blob pool signer not configured".to_string())),
            };

        let blob_tx = self
            .prepare_blob_tx(&state_data, *blob_signer_address)
            .await?;

        let tx = blob_provider.send_transaction(blob_tx).await?;

        Ok(tx.tx_hash().0)
    }

    #[cfg(feature = "test-helpers")]
    async fn finalized(&self, block: ValidatedFuelBlock) -> Result<bool> {
        Ok(self
            .contract
            .finalized(block.hash().into(), U256::from(block.height()))
            .call()
            .await?
            ._0)
    }

    #[cfg(feature = "test-helpers")]
    async fn block_hash_at_commit_height(&self, commit_height: u32) -> Result<[u8; 32]> {
        Ok(self
            .contract
            .blockHashAtCommit(U256::from(commit_height))
            .call()
            .await?
            ._0
            .into())
    }
}

impl WsConnection {
    pub async fn connect(
        url: Url,
        contract_address: Address,
        main_signer: AwsSigner,
        blob_signer: Option<AwsSigner>,
    ) -> Result<Self> {
        let address = main_signer.address();

        let ws = WsConnect::new(url);
        let provider = Self::provider_with_signer(ws.clone(), main_signer).await?;

        let (blob_provider, blob_signer_address) = if let Some(signer) = blob_signer {
            let blob_signer_address = signer.address();
            let blob_provider = Self::provider_with_signer(ws, signer).await?;
            (Some(blob_provider), Some(blob_signer_address))
        } else {
            (None, None)
        };

        let contract_address = Address::from_slice(contract_address.as_ref());
        let contract = FuelStateContract::new(contract_address, provider.clone());

        let interval_u256 = contract.BLOCKS_PER_COMMIT_INTERVAL().call().await?._0;

        let commit_interval = u32::try_from(interval_u256)
            .map_err(|e| Error::Other(e.to_string()))
            .and_then(|value| {
                NonZeroU32::new(value).ok_or_else(|| {
                    Error::Other("l1 contract reported a commit interval of 0".to_string())
                })
            })?;

        Ok(Self {
            provider,
            blob_provider,
            address,
            blob_signer_address,
            contract,
            commit_interval,
        })
    }

    async fn provider_with_signer(ws: WsConnect, signer: AwsSigner) -> Result<WsProvider> {
        let wallet = EthereumWallet::from(signer);
        ProviderBuilder::new()
            .with_recommended_fillers()
            .wallet(wallet)
            .on_ws(ws)
            .await
            .map_err(Into::into)
    }

    pub(crate) fn calculate_commit_height(block_height: u32, commit_interval: NonZeroU32) -> U256 {
        U256::from(block_height / commit_interval)
    }

    async fn _balance(&self, address: Address) -> Result<U256> {
        Ok(self.provider.get_balance(address).await?)
    }

    async fn prepare_blob_tx(&self, data: &[u8], to: Address) -> Result<TransactionRequest> {
        let sidecar = SidecarBuilder::from_coder_and_data(SimpleCoder::default(), data).build()?;

        let blob_tx = TransactionRequest::default()
            .with_to(to)
            .with_blob_sidecar(sidecar);

        Ok(blob_tx)
    }

    fn convert_to_tx_response(
        tx_receipt: Option<TransactionReceipt>,
    ) -> Result<Option<TransactionResponse>> {
        let Some(tx_receipt) = tx_receipt else {
            return Ok(None);
        };

        let block_number = Self::extract_block_number_from_receipt(&tx_receipt)?;

        Ok(Some(TransactionResponse::new(
            block_number,
            tx_receipt.status(),
        )))
    }

    fn extract_block_number_from_receipt(receipt: &TransactionReceipt) -> Result<u64> {
        receipt.block_number.ok_or_else(|| {
            Error::Other("transaction receipt does not contain block number".to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use alloy::consensus::SidecarCoder;

    use super::*;

    #[test]
    fn calculates_correctly_the_commit_height() {
        assert_eq!(
            WsConnection::calculate_commit_height(10, 3.try_into().unwrap()),
            U256::from(3)
        );
    }

    #[test]
    fn sidecarstuff() {
        let data = vec![1; 6 * 128 * 1024];
        let mut sidecar = SidecarBuilder::from_coder_and_capacity(SimpleCoder::default(), 6);

        sidecar.ingest(&data);

        let sidecar = sidecar.build().unwrap();

        // let coder = SimpleCoder::default();
        // let required_fe = coder.required_fe(data);
        // let mut this = SidecarBuilder::from_coder_and_capacity(
        //     SimpleCoder::default(),
        //     required_fe.div_ceil(alloy::eips::eip4844::FIELD_ELEMENTS_PER_BLOB as usize),
        // );

        eprintln!("{}", sidecar.blobs.len());
        panic!("kray");
    }
}
