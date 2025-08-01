use std::ops::RangeInclusive;

use alloy::providers::{
    Provider as AlloyProvider, ProviderBuilder, RootProvider,
    fillers::{BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller},
};
use services::{
    fees::SequentialBlockFees,
    types::{DateTime, Utc},
};
use tracing::info;

use crate::fee_api_helpers::batch_requests;

type InnerProvider = FillProvider<
    JoinFill<
        alloy::providers::Identity,
        JoinFill<GasFiller, JoinFill<BlobGasFiller, JoinFill<NonceFiller, ChainIdFiller>>>,
    >,
    RootProvider,
>;

#[derive(Debug, Clone)]
pub struct Provider {
    pub(crate) provider: InnerProvider,
}

impl Provider {
    pub fn new(url: &str) -> crate::Result<Self> {
        let url = url
            .parse()
            .map_err(|e| crate::error::Error::Other(format!("invalid url: {url}: {e}")))?;
        let provider = ProviderBuilder::new().connect_http(url);

        Ok(Self { provider })
    }
}

impl Provider {
    pub async fn get_block_time(&self, block_num: u64) -> crate::Result<Option<DateTime<Utc>>> {
        let block = self
            .provider
            .get_block_by_number(alloy::eips::BlockNumberOrTag::Number(block_num))
            .await
            .map_err(|e| {
                crate::error::Error::Other(format!("failed to get block by number: {e}"))
            })?;

        let time = block.and_then(|block| {
            let timestamp = block.header.timestamp;
            DateTime::<Utc>::from_timestamp(timestamp as i64, 0)
        });

        Ok(time)
    }
}
impl services::fees::Api for Provider {
    async fn fees(&self, height_range: RangeInclusive<u64>) -> crate::Result<SequentialBlockFees> {
        info!("Fetching fees for range: {:?}", height_range);
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
