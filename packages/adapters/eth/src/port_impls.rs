use std::{num::NonZeroU32, ops::RangeInclusive};

use services::{
    state_committer::port::l1::Priority,
    types::{
        Address, BlockSubmissionTx, Fragment, FragmentsSubmitted, L1Height, L1Tx, NonEmpty,
        TransactionResponse, U256,
    },
};

use crate::{FailoverClient, L1Provider, failover_client::ProviderInit, fee_api_helpers};

impl<I> services::block_committer::port::l1::Contract for FailoverClient<I>
where
    I: ProviderInit<Provider: L1Provider>,
{
    fn commit_interval(&self) -> NonZeroU32 {
        L1Provider::commit_interval(&self)
    }

    async fn submit(&self, hash: [u8; 32], height: u32) -> services::Result<BlockSubmissionTx> {
        Ok(L1Provider::submit(&self, hash, height).await?)
    }
}

impl<I> services::wallet_balance_tracker::port::l1::Api for FailoverClient<I>
where
    I: ProviderInit<Provider: L1Provider>,
{
    async fn balance(&self, address: Address) -> services::Result<U256> {
        Ok(L1Provider::balance(&self, address).await?)
    }
}

impl<I> services::block_committer::port::l1::Api for FailoverClient<I>
where
    I: ProviderInit<Provider: L1Provider>,
{
    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> services::Result<Option<TransactionResponse>> {
        Ok(L1Provider::get_transaction_response(&self, tx_hash).await?)
    }

    async fn get_block_number(&self) -> services::Result<L1Height> {
        let block_num = L1Provider::get_block_number(&self).await?;
        let height = L1Height::try_from(block_num)?;

        Ok(height)
    }
}

impl<I> services::fees::Api for FailoverClient<I>
where
    I: ProviderInit<Provider: L1Provider>,
{
    async fn current_height(&self) -> services::Result<u64> {
        Ok(self.get_block_number().await?)
    }

    async fn fees(
        &self,
        height_range: RangeInclusive<u64>,
    ) -> services::Result<services::fees::SequentialBlockFees> {
        let fees = fee_api_helpers::batch_requests(
            height_range,
            move |sub_range, percentiles| async move {
                L1Provider::fees(&self, sub_range, percentiles).await
            },
        )
        .await?;

        Ok(fees)
    }
}

impl<I> services::state_committer::port::l1::Api for FailoverClient<I>
where
    I: ProviderInit<Provider: L1Provider>,
{
    async fn current_height(&self) -> services::Result<u64> {
        Ok(self.get_block_number().await?)
    }

    async fn submit_state_fragments(
        &self,
        fragments: NonEmpty<Fragment>,
        previous_tx: Option<services::types::L1Tx>,
        priority: Priority,
    ) -> services::Result<(L1Tx, FragmentsSubmitted)> {
        Ok(L1Provider::submit_state_fragments(&self, fragments, previous_tx, priority).await?)
    }
}

impl<I> services::state_listener::port::l1::Api for FailoverClient<I>
where
    I: ProviderInit<Provider: L1Provider>,
{
    async fn is_squeezed_out(&self, tx_hash: [u8; 32]) -> services::Result<bool> {
        Ok(L1Provider::is_squeezed_out(&self, tx_hash).await?)
    }

    async fn get_transaction_response(
        &self,
        tx_hash: [u8; 32],
    ) -> services::Result<Option<TransactionResponse>> {
        Ok(L1Provider::get_transaction_response(&self, tx_hash).await?)
    }

    async fn get_block_number(&self) -> services::Result<L1Height> {
        let block_num = L1Provider::get_block_number(&self).await?;
        let height = L1Height::try_from(block_num)?;

        Ok(height)
    }

    async fn note_tx_failure(&self, reason: &str) -> services::Result<()> {
        Ok(self.note_tx_failure(reason).await?)
    }
}
