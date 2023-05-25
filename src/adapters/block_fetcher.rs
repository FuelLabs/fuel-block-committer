use actix_web::dev::Url;
use async_trait::async_trait;
use fuels::{prelude::Provider, types::block::Block};

use crate::errors::{Error, Result};

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait BlockFetcher {
    async fn latest_block(&self) -> Result<Block>;
}

pub struct FuelBlockFetcher {
    provider: Provider,
}

impl FuelBlockFetcher {
    pub async fn connect(url: Url) -> Result<Self> {
        Ok(Self {
            provider: Provider::connect(url.uri().to_string()).await.unwrap(),
        })
    }
}

#[async_trait::async_trait]
impl BlockFetcher for FuelBlockFetcher {
    async fn latest_block(&self) -> Result<Block> {
        self.provider
            .chain_info()
            .await
            .map_err(|err| match err {
                fuels::prelude::ProviderError::ClientRequestError(err) => Error::from(err),
            })
            .map(|chain_info| chain_info.latest_block)
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::Uri;
    use fuels::test_helpers::{setup_test_provider, Config};

    use super::*;

    #[tokio::test]
    async fn can_fetch_latest_block() {
        // given
        let node_config = Config {
            manual_blocks_enabled: true,
            ..Config::local_node()
        };

        let (provider, addr) =
            setup_test_provider(vec![], vec![], Some(node_config), Some(Default::default())).await;
        provider.produce_blocks(5, None).await.unwrap();

        let uri = Uri::builder()
            .path_and_query(addr.to_string())
            .build()
            .unwrap();

        let block_fetcher = FuelBlockFetcher::connect(Url::new(uri)).await.unwrap();

        // when
        let result = block_fetcher.latest_block().await.unwrap();

        // then
        assert_eq!(result.header.height, 5);
    }
}
