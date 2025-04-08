use std::sync::Arc;

use ::metrics::{RegistersMetrics, prometheus::core::Collector};
use alloy::primitives::Address;

use super::{WebsocketClient, config::TxConfig};
use crate::{Endpoint, Error, ProviderInit, Result, websocket::metrics::Metrics};
#[derive(Clone)]
pub struct WebsocketClientFactory {
    contract_address: Address,
    signers: crate::websocket::config::Signers,
    tx_config: TxConfig,
    metrics: Metrics,
}

impl WebsocketClientFactory {
    pub fn new(
        contract_address: Address,
        signers: crate::websocket::config::Signers,
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

    async fn initialize(&self, config: &Endpoint) -> Result<Arc<Self::Provider>> {
        let contract_address = self.contract_address;
        let signers = self.signers.clone();
        let tx_config = self.tx_config.clone();
        let url = config.url.clone();

        let mut client =
            WebsocketClient::connect(url, contract_address, signers.clone(), tx_config)
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
