use std::ops::RangeInclusive;

use alloy::{
    providers::{Provider as AlloyProvider, ProviderBuilder, RootProvider},
    transports::http::{Client, Http},
};
use services::historical_fees::port::l1::SequentialBlockFees;

use crate::fee_api_helpers::batch_requests;

#[derive(Debug, Clone)]
pub struct Provider {
    pub(crate) provider: RootProvider<Http<Client>>,
}

impl Provider {
    pub fn new(url: &str) -> crate::Result<Self> {
        let url = url
            .parse()
            .map_err(|e| crate::error::Error::Other(format!("invalid url: {url}: {e}")))?;
        let provider = ProviderBuilder::new().on_http(url);

        Ok(Self { provider })
    }
}

impl services::historical_fees::port::l1::Api for Provider {
    async fn fees(&self, height_range: RangeInclusive<u64>) -> crate::Result<SequentialBlockFees> {
        batch_requests(height_range, |sub_range, percentiles| async move {
            let last_block = *sub_range.end();
            let block_count = sub_range.count() as u64;
            let fees = self
                .provider
                .get_fee_history(
                    block_count,
                    alloy::eips::BlockNumberOrTag::Number(last_block),
                    percentiles,
                )
                .await
                .map_err(|e| services::Error::Network(format!("failed to get fee history: {e}")))?;

            Ok(fees)
        })
        .await
    }
    async fn current_height(&self) -> crate::Result<u64> {
        self.provider
            .get_block_number()
            .await
            .map_err(|e| services::Error::Network(format!("failed to get block number: {e}")))
    }
}
