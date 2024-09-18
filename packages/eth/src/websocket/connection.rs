use std::num::{NonZeroU32, NonZeroUsize};

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
use ports::{
    l1::GasPrices,
    types::{NonEmptyVec, TransactionResponse, ValidatedFuelBlock},
};
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
    fn max_bytes_per_submission(&self) -> std::num::NonZeroUsize {
        blob_calculations::ENCODABLE_BYTES_PER_TX
            .try_into()
            .expect("always positive")
    }
    fn gas_usage_to_store_data(&self, num_bytes: NonZeroUsize) -> ports::l1::GasUsage {
        blob_calculations::gas_usage_to_store_data(num_bytes)
    }

    async fn gas_prices(&self) -> Result<GasPrices> {
        let normal_price = self.provider.get_gas_price().await?;
        let blob_price = self.provider.get_blob_base_fee().await?;

        Ok(GasPrices {
            storage: blob_price,
            normal: normal_price,
        })
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

    async fn submit_l2_state(&self, state_data: NonEmptyVec<u8>) -> Result<[u8; 32]> {
        let (blob_provider, blob_signer_address) =
            match (&self.blob_provider, &self.blob_signer_address) {
                (Some(provider), Some(address)) => (provider, address),
                _ => return Err(Error::Other("blob pool signer not configured".to_string())),
            };

        let blob_tx = self
            .prepare_blob_tx(state_data.inner(), *blob_signer_address)
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

mod blob_calculations {
    use std::num::NonZeroUsize;

    use alloy::eips::eip4844::{
        DATA_GAS_PER_BLOB, FIELD_ELEMENTS_PER_BLOB, FIELD_ELEMENT_BYTES, MAX_BLOBS_PER_BLOCK,
        MAX_DATA_GAS_PER_BLOCK,
    };
    use itertools::Itertools;
    use ports::{l1::GasUsage, types::NonEmptyVec};

    /// Intrinsic gas cost of a eth transaction.
    const BASE_TX_COST: u64 = 21_000;

    pub(crate) fn gas_usage_to_store_data(num_bytes: NonZeroUsize) -> GasUsage {
        let num_bytes =
            u64::try_from(num_bytes.get()).expect("to not have more than u64::MAX of storage data");

        // Taken from the SimpleCoder impl
        let required_fe = num_bytes.div_ceil(31).saturating_add(1);

        // alloy constants not used since they are u64
        let blob_num = required_fe.div_ceil(FIELD_ELEMENTS_PER_BLOB);

        const MAX_BLOBS_PER_BLOCK: u64 = MAX_DATA_GAS_PER_BLOCK / DATA_GAS_PER_BLOB;
        let number_of_txs = blob_num.div_ceil(MAX_BLOBS_PER_BLOCK);

        let storage = blob_num.saturating_mul(DATA_GAS_PER_BLOB);
        let normal = number_of_txs * BASE_TX_COST;

        GasUsage { storage, normal }
    }

    // 1 whole field element is lost plus a byte for every remaining field element
    pub(crate) const ENCODABLE_BYTES_PER_TX: usize = (FIELD_ELEMENT_BYTES as usize - 1)
        * (FIELD_ELEMENTS_PER_BLOB as usize * MAX_BLOBS_PER_BLOCK - 1);

    pub(crate) fn split_into_submittable_fragments(
        data: &NonEmptyVec<u8>,
    ) -> crate::error::Result<NonEmptyVec<NonEmptyVec<u8>>> {
        Ok(data
            .iter()
            .chunks(ENCODABLE_BYTES_PER_TX)
            .into_iter()
            .fold(Vec::new(), |mut acc, chunk| {
                let bytes = chunk.copied().collect::<Vec<_>>();

                let non_empty_bytes = NonEmptyVec::try_from(bytes)
                    .expect("chunk is non-empty since it came from a non-empty vec");
                acc.push(non_empty_bytes);
                acc
            })
            .try_into()
            .expect("must have at least one fragment since the input is non-empty"))
    }

    #[cfg(test)]
    mod tests {
        use alloy::consensus::{SidecarBuilder, SimpleCoder};
        use rand::{rngs::SmallRng, Rng, SeedableRng};
        use test_case::test_case;

        use super::*;

        #[test_case(100, 1, 1; "single eth tx with one blob")]
        #[test_case(129 * 1024, 1, 2; "single eth tx with two blobs")]
        #[test_case(257 * 1024, 1, 3; "single eth tx with three blobs")]
        #[test_case(385 * 1024, 1, 4; "single eth tx with four blobs")]
        #[test_case(513 * 1024, 1, 5; "single eth tx with five blobs")]
        #[test_case(740 * 1024, 1, 6; "single eth tx with six blobs")]
        #[test_case(768 * 1024, 2, 7; "two eth tx with seven blobs")]
        #[test_case(896 * 1024, 2, 8; "two eth tx with eight blobs")]
        fn gas_usage_for_data_storage(num_bytes: usize, num_txs: usize, num_blobs: usize) {
            // given

            // when
            let usage = gas_usage_to_store_data(num_bytes.try_into().unwrap());

            // then
            assert_eq!(usage.normal as usize, num_txs * 21_000);
            assert_eq!(
                usage.storage as u64,
                num_blobs as u64 * alloy::eips::eip4844::DATA_GAS_PER_BLOB
            );

            let mut rng = SmallRng::from_seed([0; 32]);
            let mut data = vec![0; num_bytes];
            rng.fill(&mut data[..]);

            let mut builder = SidecarBuilder::from_coder_and_capacity(SimpleCoder::default(), 0);
            builder.ingest(&data);

            assert_eq!(builder.build().unwrap().blobs.len(), num_blobs,);
        }

        #[test_case(100; "one small fragment")]
        #[test_case(1000000; "one full fragment and one small")]
        #[test_case(2000000; "two full fragments and one small")]
        fn splits_into_correct_fragments_that_can_fit_in_a_tx(num_bytes: usize) {
            // given
            let mut rng = SmallRng::from_seed([0; 32]);
            let mut bytes = vec![0; num_bytes];
            rng.fill(&mut bytes[..]);
            let original_bytes = bytes.try_into().unwrap();

            // when
            let fragments = split_into_submittable_fragments(&original_bytes).unwrap();

            // then
            let reconstructed = fragments
                .inner()
                .iter()
                .flat_map(|f| f.inner())
                .copied()
                .collect_vec();
            assert_eq!(original_bytes.inner(), &reconstructed);

            for (idx, fragment) in fragments.inner().iter().enumerate() {
                let mut builder =
                    SidecarBuilder::from_coder_and_capacity(SimpleCoder::default(), 0);
                builder.ingest(fragment.inner());
                let num_blobs = builder.build().unwrap().blobs.len();

                if idx == fragments.len().get() - 1 {
                    assert!(num_blobs <= 6);
                } else {
                    assert_eq!(num_blobs, 6);
                }
            }
        }

        #[test]
        fn encodable_bytes_per_tx_correctly_calculated() {
            let mut rand_gen = SmallRng::from_seed([0; 32]);
            let mut max_bytes = [0; ENCODABLE_BYTES_PER_TX];
            rand_gen.fill(&mut max_bytes[..]);

            let mut builder = SidecarBuilder::from_coder_and_capacity(SimpleCoder::default(), 6);
            builder.ingest(&max_bytes);

            assert_eq!(builder.build().unwrap().blobs.len(), 6);

            let mut one_too_many = [0; ENCODABLE_BYTES_PER_TX + 1];
            rand_gen.fill(&mut one_too_many[..]);
            let mut builder = SidecarBuilder::from_coder_and_capacity(SimpleCoder::default(), 6);
            builder.ingest(&one_too_many);

            assert_eq!(builder.build().unwrap().blobs.len(), 7);
        }
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
    use super::*;

    #[test]
    fn calculates_correctly_the_commit_height() {
        assert_eq!(
            WsConnection::calculate_commit_height(10, 3.try_into().unwrap()),
            U256::from(3)
        );
    }
}
