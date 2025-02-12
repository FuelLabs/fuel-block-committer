pub mod service {
    use futures::{StreamExt, TryStreamExt};
    use tracing::info;

    use crate::{
        types::{nonempty, CompressedFuelBlock, NonEmpty},
        Result, Runner,
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
    }

    impl<Db, FuelApi> BlockImporter<Db, FuelApi> {
        /// Creates a new `BlockImporter`.
        pub fn new(storage: Db, fuel_api: FuelApi, lookback_window: u32) -> Self {
            Self {
                storage,
                fuel_api,
                lookback_window,
                max_blocks: 3600,
                max_size: 20_000_000,
            }
        }
    }

    impl<Db, FuelApi> BlockImporter<Db, FuelApi>
    where
        Db: crate::block_importer::port::Storage,
        FuelApi: crate::block_importer::port::fuel::Api,
    {
        async fn import_blocks(&self, blocks: NonEmpty<CompressedFuelBlock>) -> Result<()> {
            let starting_height = blocks.first().height;
            let ending_height = blocks.last().height;

            self.storage.insert_blocks(blocks).await?;

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

            for range in self
                .storage
                .missing_blocks(starting_height, chain_height)
                .await?
            {
                let mut block_stream = self
                    .fuel_api
                    .compressed_blocks_in_height_range(range)
                    .map_err(crate::Error::from)
                    .try_chunks(100)
                    .map(|res| match res {
                        Ok(blocks) => (blocks, None),
                        Err(err) => (err.0, Some(err.1)),
                    });

                while let Some((blocks_until_error, maybe_err)) = block_stream.next().await {
                    if let Some(blocks) = NonEmpty::from_vec(blocks_until_error) {
                        self.import_blocks(blocks).await?;
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

    use crate::{types::CompressedFuelBlock, Result};

    #[allow(async_fn_in_trait)]
    #[trait_variant::make(Send)]
    pub trait Storage: Sync {
        async fn insert_blocks(&self, block: NonEmpty<CompressedFuelBlock>) -> Result<()>;
        async fn missing_blocks(
            &self,
            starting_height: u32,
            current_height: u32,
        ) -> Result<Vec<RangeInclusive<u32>>>;
    }

    pub mod fuel {
        use std::ops::RangeInclusive;

        use futures::stream::BoxStream;

        use crate::{types::CompressedFuelBlock, Result};

        #[allow(async_fn_in_trait)]
        #[trait_variant::make(Send)]
        #[cfg_attr(feature = "test-helpers", mockall::automock)]
        pub trait Api: Sync {
            fn compressed_blocks_in_height_range(
                &self,
                range: RangeInclusive<u32>,
            ) -> BoxStream<'_, Result<CompressedFuelBlock>>;
            async fn latest_height(&self) -> Result<u32>;
        }
    }
}
