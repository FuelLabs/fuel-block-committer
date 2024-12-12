use std::{ops::RangeInclusive, time::Duration};

use crate::historical_fees::port::{l1::FeesProvider, service::HistoricalFeesProvider};
use nonempty::NonEmpty;
use rayon::range_inclusive;

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub short_term_sma_num_blocks: u32,
    pub long_term_sma_num_blocks: u32,
}

pub struct SendOrWaitDecider<P> {
    price_service: HistoricalFeesProvider<P>,
    config: Config,
}

impl<P> SendOrWaitDecider<P> {
    pub fn new(price_service: HistoricalFeesProvider<P>, config: Config) -> Self {
        Self {
            price_service,
            config,
        }
    }
}

impl<P: FeesProvider> SendOrWaitDecider<P> {
    pub async fn should_send_blob_tx(&self) -> bool {
        let short_term_sma = self
            .price_service
            .calculate_sma(self.config.short_term_sma_num_blocks)
            .await;

        let long_term_sma = self
            .price_service
            .calculate_sma(self.config.long_term_sma_num_blocks)
            .await;

        return short_term_sma.base_fee_per_gas < long_term_sma.base_fee_per_gas;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet, HashMap},
        time::Duration,
    };

    use crate::historical_fees::port::{
        l1::testing::{incrementing_fees, TestFeesProvider},
        BlockFees, Fees,
    };

    use crate::types::CollectNonEmpty;

    use super::*;

    #[tokio::test]
    async fn should_send_if_shortterm_sma_lower_than_longterm_sma() {
        // given
        let config = Config {
            short_term_sma_num_blocks: 5,
            long_term_sma_num_blocks: 50,
        };

        let fees_provider = TestFeesProvider::new(incrementing_fees(50));
        let price_service = HistoricalFeesProvider::new(fees_provider);

        let sut = SendOrWaitDecider::new(price_service, config);

        // when
        let decision = sut.should_send_blob_tx().await;

        // then
        assert!(decision, "Should have sent");
    }
}
