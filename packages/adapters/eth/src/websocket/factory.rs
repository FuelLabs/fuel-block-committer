use std::sync::Arc;

use crate::{
    Error, ProviderConfig, Result,
    failover_client::ProviderInit,
    websocket::metrics::Metrics,
};
use ::metrics::{RegistersMetrics, prometheus::core::Collector};
use alloy::primitives::Address;
use url::Url;

use super::{TxConfig, WebsocketClient};
#[derive(Clone)]
pub struct WebsocketClientFactory {
    contract_address: Address,
    signers: Arc<crate::websocket::Signers>,
    tx_config: TxConfig,
    metrics: Metrics,
}

impl WebsocketClientFactory {
    pub fn new(
        contract_address: Address,
        signers: Arc<crate::websocket::Signers>,
        tx_config: TxConfig,
    ) -> Self {
        Self {
            contract_address,
            signers,
            tx_config,
            metrics: Default::default(),
        }
    }
}

impl ProviderInit for WebsocketClientFactory {
    type Provider = WebsocketClient;

    async fn initialize(&self, config: &ProviderConfig) -> Result<Arc<Self::Provider>> {
        let contract_address = self.contract_address;
        let signers = self.signers.clone();
        let tx_config = self.tx_config.clone();
        let url_str = config.url.clone();

        let url = Url::parse(&url_str).map_err(|e| Error::Other(format!("Invalid URL: {}", e)))?;

        let mut client =
            WebsocketClient::connect(url, contract_address, (*signers).clone(), tx_config)
                .await
                .map_err(|e| Error::Other(format!("Failed to connect: {}", e)))?;

        client.set_metrics(self.metrics.clone());

        Ok(Arc::new(client))
    }
}

impl RegistersMetrics for WebsocketClientFactory {
    fn metrics(&self) -> Vec<Box<dyn Collector>> {
        vec![
            Box::new(self.metrics.blobs_per_tx.clone()),
            Box::new(self.metrics.blob_unused_bytes.clone()),
        ]
    }
}
