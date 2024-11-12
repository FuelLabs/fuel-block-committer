use std::{num::NonZeroUsize, time::Duration};

use crate::{
    ports::{
        clock::Clock,
        storage::{BundleFragment, Storage},
    },
    types::{CollectNonEmpty, DateTime, L1Tx, NonEmpty, Utc},
    Result, Runner,
};
use itertools::Itertools;
use tracing::info;

// src/config.rs
#[derive(Debug, Clone)]
pub struct Config {
    /// The lookback window in blocks to determine the starting height.
    pub lookback_window: u32,
    pub fragment_accumulation_timeout: Duration,
    pub fragments_to_accumulate: NonZeroUsize,
    pub gas_bump_timeout: Duration,
    pub tx_max_fee: u128,
}

#[cfg(feature = "test-helpers")]
impl Default for Config {
    fn default() -> Self {
        Self {
            lookback_window: 1000,
            fragment_accumulation_timeout: Duration::from_secs(0),
            fragments_to_accumulate: 1.try_into().unwrap(),
            gas_bump_timeout: Duration::from_secs(300),
            tx_max_fee: 1_000_000_000,
        }
    }
}

/// The `StateCommitter` is responsible for committing state fragments to L1.
pub struct StateCommitter<L1, F, Storage, C> {
    l1_adapter: L1,
    fuel_api: F,
    storage: Storage,
    config: Config,
    clock: C,
    startup_time: DateTime<Utc>,
}

impl<L1, F, S, C> StateCommitter<L1, F, S, C>
where
    C: Clock,
{
    /// Creates a new `StateCommitter`.
    pub fn new(l1_adapter: L1, fuel_api: F, storage: S, config: Config, clock: C) -> Self {
        let startup_time = clock.now();
        Self {
            l1_adapter,
            fuel_api,
            storage,
            config,
            clock,
            startup_time,
        }
    }
}

impl<L1, F, Db, C> StateCommitter<L1, F, Db, C>
where
    L1: crate::ports::l1::Api,
    F: crate::ports::fuel::Api,
    Db: Storage,
    C: Clock,
{
    async fn get_reference_time(&self) -> Result<DateTime<Utc>> {
        Ok(self
            .storage
            .last_time_a_fragment_was_finalized()
            .await?
            .unwrap_or(self.startup_time))
    }

    async fn is_timeout_expired(&self) -> Result<bool> {
        let reference_time = self.get_reference_time().await?;
        let elapsed = self.clock.now() - reference_time;
        let std_elapsed = elapsed
            .to_std()
            .map_err(|e| crate::Error::Other(format!("Failed to convert time: {}", e)))?;
        Ok(std_elapsed >= self.config.fragment_accumulation_timeout)
    }

    async fn submit_fragments(
        &self,
        fragments: NonEmpty<BundleFragment>,
        previous_tx: Option<L1Tx>,
    ) -> Result<()> {
        info!("about to send at most {} fragments", fragments.len());

        let data = fragments.clone().map(|f| f.fragment);

        match self
            .l1_adapter
            .submit_state_fragments(data, previous_tx)
            .await
        {
            Ok((submitted_tx, submitted_fragments)) => {
                let fragment_ids = fragments
                    .iter()
                    .map(|f| f.id)
                    .take(submitted_fragments.num_fragments.get())
                    .collect_nonempty()
                    .expect("non-empty vec");

                let ids = fragment_ids
                    .iter()
                    .map(|id| id.as_u32().to_string())
                    .join(", ");

                let tx_hash = submitted_tx.hash;
                self.storage
                    .record_pending_tx(submitted_tx, fragment_ids, self.clock.now())
                    .await?;

                tracing::info!("Submitted fragments {ids} with tx {}", hex::encode(tx_hash));
                Ok(())
            }
            Err(e) => {
                let ids = fragments
                    .iter()
                    .map(|f| f.id.as_u32().to_string())
                    .join(", ");

                tracing::error!("Failed to submit fragments {ids}: {e}");
                Err(e.into())
            }
        }
    }

    async fn latest_pending_transaction(&self) -> Result<Option<L1Tx>> {
        let tx = self.storage.get_latest_pending_txs().await?;
        Ok(tx)
    }

    async fn next_fragments_to_submit(&self) -> Result<Option<NonEmpty<BundleFragment>>> {
        let latest_height = self.fuel_api.latest_height().await?;
        let starting_height = latest_height.saturating_sub(self.config.lookback_window);

        // although we shouldn't know at this layer how many fragments the L1 can accept, we ignore
        // this for now and put the eth value of max blobs per block (6).
        let existing_fragments = self
            .storage
            .oldest_nonfinalized_fragments(starting_height, 6)
            .await?;

        Ok(NonEmpty::collect(existing_fragments))
    }

    async fn should_submit_fragments(&self, fragment_count: NonZeroUsize) -> Result<bool> {
        if fragment_count >= self.config.fragments_to_accumulate {
            return Ok(true);
        }
        info!(
            "have only {} out of the target {} fragments per tx",
            fragment_count, self.config.fragments_to_accumulate
        );

        let expired = self.is_timeout_expired().await?;
        if expired {
            info!(
                "fragment accumulation timeout expired, proceeding with {} fragments",
                fragment_count
            );
        }

        Ok(expired)
    }

    async fn submit_fragments_if_ready(&self) -> Result<()> {
        if let Some(fragments) = self.next_fragments_to_submit().await? {
            if self
                .should_submit_fragments(fragments.len_nonzero())
                .await?
            {
                self.submit_fragments(fragments, None).await?;
            }
        }
        Ok(())
    }

    fn elapsed_since_tx_submitted(&self, tx: &L1Tx) -> Result<Duration> {
        let created_at = tx.created_at.expect("tx to have timestamp");
        let elapsed = self.clock.elapsed(created_at);

        Ok(elapsed?)
    }

    async fn fragments_submitted_by_tx(
        &self,
        tx_hash: [u8; 32],
    ) -> Result<NonEmpty<BundleFragment>> {
        let fragments = self.storage.fragments_submitted_by_tx(tx_hash).await?;

        match NonEmpty::collect(fragments) {
            Some(fragments) => Ok(fragments),
            None => Err(crate::Error::Other(format!(
                "no fragments found for previously submitted tx {}",
                hex::encode(tx_hash)
            ))),
        }
    }

    async fn resubmit_fragments_if_stalled(&self) -> Result<()> {
        let Some(previous_tx) = self.latest_pending_transaction().await? else {
            return Ok(());
        };

        let elapsed = self.elapsed_since_tx_submitted(&previous_tx)?;

        if elapsed >= self.config.gas_bump_timeout {
            info!(
                "replacing tx {} because it was pending for {}s",
                hex::encode(previous_tx.hash),
                elapsed.as_secs()
            );

            let fragments = self.fragments_submitted_by_tx(previous_tx.hash).await?;
            self.submit_fragments(fragments, Some(previous_tx)).await?;
        }

        Ok(())
    }
}

impl<L1, F, Db, C> Runner for StateCommitter<L1, F, Db, C>
where
    F: crate::ports::fuel::Api + Send + Sync,
    L1: crate::ports::l1::Api + Send + Sync,
    Db: Storage + Clone + Send + Sync,
    C: Clock + Send + Sync,
{
    async fn run(&mut self) -> Result<()> {
        if self.storage.has_nonfinalized_txs().await? {
            self.resubmit_fragments_if_stalled().await?
        } else {
            self.submit_fragments_if_ready().await?
        };

        Ok(())
    }
}
