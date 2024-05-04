use fuel_core_client::client::{types::Block, FuelClient as GqlClient};
use metrics::ConnectionHealthTracker;
use url::Url;

use crate::{metrics::Metrics, Error, Result};

pub struct Client {
    pub(crate) client: GqlClient,
    pub(crate) metrics: Metrics,
    pub(crate) health_tracker: ConnectionHealthTracker,
}

impl Client {
    pub fn new(url: &Url, unhealthy_after_n_errors: usize) -> Self {
        let client = GqlClient::new(url).expect("Url to be well formed");
        Self {
            client,
            metrics: Metrics::default(),
            health_tracker: ConnectionHealthTracker::new(unhealthy_after_n_errors),
        }
    }

    #[cfg(feature = "test-helpers")]
    pub async fn produce_blocks(&self, num: u32) -> Result<()> {
        self.client
            .produce_blocks(num, None)
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        Ok(())
    }

    pub(crate) async fn _block_at_height(&self, height: u32) -> Result<Option<Block>> {
        let maybe_block = self
            .client
            .block_by_height(height.into())
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        Ok(maybe_block.map(Into::into))
    }

    pub(crate) async fn _latest_block(&self) -> Result<Block> {
        match self.client.chain_info().await {
            Ok(chain_info) => {
                self.handle_network_success();
                Ok(chain_info.latest_block)
            }
            Err(err) => {
                self.handle_network_error();
                Err(Error::Network(err.to_string()))
            }
        }
    }
}
