mod chunking;
pub mod service {

    use std::time::Instant;

    use futures::{StreamExt, TryStreamExt};
    use metrics::{
        RegistersMetrics,
        prometheus::{Histogram, IntGauge, histogram_opts, linear_buckets},
    };
    use tracing::info;

    use super::chunking::TryChunkBlocksExt;
    use crate::{
        Result, Runner,
        types::{CompressedFuelBlock, NonEmpty},
    };

    /// The `BlockImporter` is responsible for importing blocks from the Fuel blockchain
    /// into local storage. It fetches blocks from the Fuel API
    /// and stores them if they are not already present.
    pub struct BlockImporter<Db, FuelApi> {
        storage: Db,
        fuel_api: FuelApi,
        lookback_window: u32,
        /// Maximum number of blocks to accumulate before importing.
        max_blocks: usize,
        /// Maximum total size (in bytes) to accumulate before importing.
        max_size: usize,
        metrics: Metrics,
    }

    #[derive(Debug, Clone)]
    struct Metrics {
        latest_imported_height: IntGauge,
        blocks_import_rate: Histogram,
        import_lag: IntGauge,
        import_latency: Histogram,
    }

    impl Default for Metrics {
        fn default() -> Self {
            let latest_imported_height = IntGauge::new(
                "latest_imported_height",
                "The height of the latest imported block",
            )
            .expect("latest_imported_height gauge to be correctly configured");

            let blocks_import_rate = Histogram::with_opts(histogram_opts!(
                "blocks_import_rate",
                "Number of blocks imported per second",
                linear_buckets(1.0, 5.0, 10).expect("to be correctly configured")
            ))
            .expect("to be correctly configured");

            let import_lag = IntGauge::new(
                "import_lag",
                "The difference between latest chain height and latest imported height",
            )
            .expect("import_lag gauge to be correctly configured");

            let import_latency = Histogram::with_opts(histogram_opts!(
                "import_latency",
                "Time it takes to import blocks in milliseconds",
                linear_buckets(10.0, 100.0, 10).expect("to be correctly configured")
            ))
            .expect("to be correctly configured");

            Self {
                latest_imported_height,
                blocks_import_rate,
                import_lag,
                import_latency,
            }
        }
    }

    impl<Db, FuelApi> RegistersMetrics for BlockImporter<Db, FuelApi> {
        fn metrics(&self) -> Vec<Box<dyn metrics::prometheus::core::Collector>> {
            vec![
                Box::new(self.metrics.latest_imported_height.clone()),
                Box::new(self.metrics.blocks_import_rate.clone()),
                Box::new(self.metrics.import_lag.clone()),
                Box::new(self.metrics.import_latency.clone()),
            ]
        }
    }

    impl<Db, FuelApi> BlockImporter<Db, FuelApi> {
        pub fn new(
            storage: Db,
            fuel_api: FuelApi,
            lookback_window: u32,
            max_blocks: usize,
            max_size: usize,
        ) -> Self {
            Self {
                storage,
                fuel_api,
                lookback_window,
                max_blocks,
                max_size,
                metrics: Metrics::default(),
            }
        }
    }

    impl<Db, FuelApi> BlockImporter<Db, FuelApi>
    where
        Db: crate::block_importer::port::Storage,
        FuelApi: crate::block_importer::port::fuel::Api,
    {
        async fn import_blocks(
            &self,
            blocks: NonEmpty<CompressedFuelBlock>,
            per_second_metrics_last_collected: &mut Instant,
        ) -> Result<()> {
            let starting_height = blocks.first().height;
            let ending_height = blocks.last().height;
            let import_start = std::time::Instant::now();

            self.storage.insert_blocks(blocks).await?;

            // Update metrics after successful import
            let import_duration = import_start.elapsed();
            self.metrics
                .import_latency
                .observe(import_duration.as_millis() as f64);
            self.metrics
                .latest_imported_height
                .set(ending_height as i64);

            // Calculate import rate (blocks per second)
            let now = Instant::now();
            let elapsed_since_last = now.duration_since(*per_second_metrics_last_collected);
            if elapsed_since_last.as_secs() > 0 {
                let blocks_count = (ending_height - starting_height + 1) as f64;
                let rate = blocks_count / elapsed_since_last.as_secs_f64();
                self.metrics.blocks_import_rate.observe(rate);
                *per_second_metrics_last_collected = now;
            }

            info!("Imported blocks: {starting_height}..={ending_height}");

            Ok(())
        }
    }

    impl<Db, FuelApi> Runner for BlockImporter<Db, FuelApi>
    where
        Db: crate::block_importer::port::Storage + Send + Sync,
        FuelApi: crate::block_importer::port::fuel::Api + Send + Sync,
    {
        async fn run(&mut self) -> Result<()> {
            let chain_height = self.fuel_api.latest_height().await?;
            let starting_height = chain_height.saturating_sub(self.lookback_window);

            // Update the import lag metric
            let latest_height = self.storage.get_latest_block_height().await?;
            let import_lag = chain_height.saturating_sub(latest_height);
            self.metrics.import_lag.set(import_lag as i64);

            for range in self
                .storage
                .missing_blocks(starting_height, chain_height)
                .await?
            {
                let mut block_stream = self
                    .fuel_api
                    .compressed_blocks_in_height_range(range)
                    .map_err(crate::Error::from)
                    .try_chunk_blocks(self.max_blocks, self.max_size)
                    .map(|res| match res {
                        Ok(blocks) => (Some(blocks), None),
                        Err(err) => (err.blocks, Some(err.error)),
                    });

                let mut per_second_metrics_last_collected = std::time::Instant::now();

                while let Some((blocks_until_potential_error, maybe_err)) =
                    block_stream.next().await
                {
                    if let Some(blocks) = blocks_until_potential_error {
                        self.import_blocks(blocks, &mut per_second_metrics_last_collected)
                            .await?;
                    }

                    if let Some(err) = maybe_err {
                        return Err(err);
                    }
                }
            }

            Ok(())
        }
    }
}

pub mod port {
    use std::ops::RangeInclusive;

    use nonempty::NonEmpty;

    use crate::{Result, types::CompressedFuelBlock};

    pub mod fuel {
        use std::ops::RangeInclusive;

        #[allow(async_fn_in_trait)]
        #[trait_variant::make(Send)]
        #[cfg_attr(feature = "test-helpers", mockall::automock)]
        pub trait Api: Sync {
            async fn latest_height(&self) -> crate::Result<u32>;
            fn compressed_blocks_in_height_range(
                &self,
                height_range: RangeInclusive<u32>,
            ) -> futures::stream::BoxStream<
                '_,
                Result<crate::types::CompressedFuelBlock, crate::Error>,
            >;
        }
    }

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Sync {
        async fn missing_blocks(
            &self,
            starting_height: u32,
            current_height: u32,
        ) -> Result<Vec<RangeInclusive<u32>>>;

        async fn insert_blocks(&self, blocks: NonEmpty<CompressedFuelBlock>) -> Result<()>;

        /// Returns the height of the latest block that has been imported
        async fn get_latest_block_height(&self) -> Result<u32>;
    }
}
