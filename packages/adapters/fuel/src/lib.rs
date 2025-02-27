#![deny(unused_crate_dependencies)]

use std::ops::RangeInclusive;

use futures::{StreamExt, stream::BoxStream};
mod client;
mod metrics;

pub use client::*;
use delegate::delegate;
use services::{Result, block_committer::port::fuel::FuelBlock};
pub use url::Url;

impl services::block_importer::port::fuel::Api for client::HttpClient {
    async fn latest_height(&self) -> Result<u32> {
        self.latest_block().await.map(|b| b.height)
    }

    fn compressed_blocks_in_height_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> BoxStream<'_, Result<services::types::CompressedFuelBlock>> {
        self._compressed_blocks_in_height_range(range).boxed()
    }
}

impl services::block_bundler::port::fuel::Api for client::HttpClient {
    async fn latest_height(&self) -> Result<u32> {
        self.latest_block().await.map(|b| b.height)
    }
}

impl services::block_committer::port::fuel::Api for client::HttpClient {
    delegate! {
        to self {
            async fn block_at_height(&self, height: u32) -> Result<Option<FuelBlock>>;
            async fn latest_block(&self) -> Result<FuelBlock>;
        }
    }
}

impl services::state_committer::port::fuel::Api for client::HttpClient {
    async fn latest_height(&self) -> Result<u32> {
        self.latest_block().await.map(|b| b.height)
    }
}

#[cfg(test)]
mod tests {
    use ::metrics::{
        RegistersMetrics,
        prometheus::{Registry, proto::Metric},
    };
    use url::Url;

    use super::*;
    use crate::client::HttpClient;

    // TODO: once a sdk release is made these can be adapted
    // #[tokio::test]
    // async fn can_fetch_latest_block() {
    //     // given
    //     let node_config = Config {
    //         debug: true,
    //         ..Default::default()
    //     };
    //
    //     let provider =
    //         setup_test_provider(vec![], vec![], Some(node_config), Some(Default::default()))
    //             .await
    //             .unwrap();
    //     provider.produce_blocks(5, None).await.unwrap();
    //
    //     let addr = provider.url();
    //     let url = Url::parse(addr).unwrap();
    //     let fuel_adapter = FuelClient::new(&url, 1);
    //
    //     // when
    //     let result = fuel_adapter.latest_block().await.unwrap();
    //
    //     // then
    //     assert_eq!(result.height, 5);
    // }

    // TODO: once a sdk release is made these can be adapted
    // #[tokio::test]
    // async fn can_fetch_block_at_height() {
    //     // given
    //     let node_config = Config {
    //         debug: true,
    //         ..Default::default()
    //     };
    //
    //     let provider =
    //         setup_test_provider(vec![], vec![], Some(node_config), Some(Default::default()))
    //             .await
    //             .unwrap();
    //     provider.produce_blocks(5, None).await.unwrap();
    //
    //     let url = Url::parse(provider.url()).unwrap();
    //
    //     let fuel_adapter = FuelClient::new(&url, 1);
    //
    //     // when
    //     let result = fuel_adapter.block_at_height(3).await.unwrap().unwrap();
    //
    //     // then
    //     assert_eq!(result.height, 3);
    // }

    #[tokio::test]
    async fn updates_metrics_in_case_of_network_err() {
        // temporary 'fake' address to cause a network error the same effect will be achieved by
        // killing the node once the SDK supports it.
        let url = Url::parse("localhost:12344").unwrap();

        let fuel_adapter = HttpClient::new(&url, 1, 1.try_into().unwrap());

        let registry = Registry::default();
        fuel_adapter.register_metrics(&registry);

        // when
        let result = fuel_adapter.latest_block().await;

        // then
        assert!(result.is_err());
        let metrics = registry.gather();
        let network_errors_metric = metrics
            .iter()
            .find(|metric| metric.get_name() == "fuel_network_errors")
            .and_then(|metric| metric.get_metric().first())
            .map(Metric::get_counter)
            .unwrap();

        assert_eq!(network_errors_metric.get_value(), 1f64);
    }

    #[tokio::test]
    async fn correctly_tracks_network_health() {
        // temporary 'fake' address to cause a network error the same effect will be achieved by
        // killing the node once the SDK supports it.
        let url = Url::parse("http://localhost:12344").unwrap();

        let fuel_adapter = client::HttpClient::new(&url, 3, 1.try_into().unwrap());
        let health_check = fuel_adapter.connection_health_checker();

        assert!(health_check.healthy());

        let _ = fuel_adapter.latest_block().await;
        assert!(health_check.healthy());

        let _ = fuel_adapter.latest_block().await;
        assert!(health_check.healthy());

        let _ = fuel_adapter.latest_block().await;
        assert!(!health_check.healthy());
    }
}
