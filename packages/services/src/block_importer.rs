pub mod service {

    mod ext {
        use futures::stream::Stream;
        use futures::task::{Context, Poll};
        use std::pin::Pin;

        use crate::types::{CompressedFuelBlock, NonEmpty};

        /// The error type produced by our chunking combinator when an error is encountered.
        /// It carries any blocks that were accumulated before the error, plus the error itself.
        #[derive(Debug)]
        pub struct ChunkError<E> {
            pub blocks: Option<NonEmpty<CompressedFuelBlock>>,
            pub error: E,
        }

        /// Extension trait that adds the `try_chunk_blocks` method.
        pub trait TryChunkBlocksExt<E>:
            Stream<Item = Result<CompressedFuelBlock, E>> + Sized
        {
            /// Chunk the stream so that each yielded item is either a full chunk of blocks
            /// (i.e. up to `num_blocks` items or accumulating at most `max_size` bytes)
            /// or (if an error occurs) an error value carrying any blocks accumulated so far.
            ///
            /// Note that in this version we do not yield a partial chunk merely because the
            /// underlying stream returns Pending—rather we wait until more blocks arrive.
            fn try_chunk_blocks(self, num_blocks: usize, max_size: usize) -> TryChunkBlocks<Self>;
        }

        impl<S, E> TryChunkBlocksExt<E> for S
        where
            S: Stream<Item = Result<CompressedFuelBlock, E>> + Sized,
        {
            fn try_chunk_blocks(self, num_blocks: usize, max_size: usize) -> TryChunkBlocks<Self> {
                TryChunkBlocks::new(self, num_blocks, max_size)
            }
        }

        /// Our stream combinator that accumulates blocks into larger chunks.
        pub struct TryChunkBlocks<S> {
            stream: S,
            num_blocks: usize,
            max_size: usize,
            /// A block that did not “fit” in the previous chunk.
            leftover: Option<CompressedFuelBlock>,
            /// Accumulated blocks for the current chunk.
            current_chunk: Vec<CompressedFuelBlock>,
            /// Total size (in bytes) of the accumulated blocks.
            accumulated_size: usize,
            /// True once we have determined that the underlying stream is finished.
            finished: bool,
        }

        impl<S> TryChunkBlocks<S> {
            pub fn new(stream: S, num_blocks: usize, max_size: usize) -> Self {
                Self {
                    stream,
                    num_blocks,
                    max_size,
                    leftover: None,
                    current_chunk: Vec::with_capacity(num_blocks),
                    accumulated_size: 0,
                    finished: false,
                }
            }
        }

        impl<S> Unpin for TryChunkBlocks<S> {}

        impl<S, E> Stream for TryChunkBlocks<S>
        where
            S: Stream<Item = Result<CompressedFuelBlock, E>> + Unpin,
        {
            // Each item is either a successful chunk or an error (with partial chunk)
            type Item = Result<NonEmpty<CompressedFuelBlock>, ChunkError<E>>;

            fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                let this = self.get_mut();

                if this.finished {
                    // If we already finished, yield any final accumulated chunk (if any) and then end.
                    if !this.current_chunk.is_empty() {
                        let chunk = std::mem::take(&mut this.current_chunk);
                        this.accumulated_size = 0;
                        if let Some(non_empty) = NonEmpty::from_vec(chunk) {
                            return Poll::Ready(Some(Ok(non_empty)));
                        }
                    }
                    return Poll::Ready(None);
                }

                // If we have a leftover block from before, add it first.
                if let Some(block) = this.leftover.take() {
                    this.accumulated_size += block.data.len();
                    this.current_chunk.push(block);
                }

                // Now try to pull items from the underlying stream until we reach a threshold.
                loop {
                    // If we've reached one of our thresholds, break to yield the chunk.
                    if !this.current_chunk.is_empty()
                        && (this.current_chunk.len() >= this.num_blocks
                            || this.accumulated_size >= this.max_size)
                    {
                        break;
                    }

                    match Pin::new(&mut this.stream).poll_next(cx) {
                        Poll::Pending => {
                            // Instead of yielding a partial chunk when Pending,
                            // we simply return Pending so that we continue accumulation.
                            return Poll::Pending;
                        }
                        Poll::Ready(Some(item)) => {
                            match item {
                                Ok(block) => {
                                    let block_size = block.data.len();
                                    // If adding this block would exceed a threshold (and we already have items),
                                    // then store it as leftover and break.
                                    if !this.current_chunk.is_empty()
                                        && ((this.current_chunk.len() + 1) > this.num_blocks
                                            || (this.accumulated_size + block_size) > this.max_size)
                                    {
                                        this.leftover = Some(block);
                                        break;
                                    }
                                    // Otherwise, add the block.
                                    this.accumulated_size += block_size;
                                    this.current_chunk.push(block);
                                }
                                Err(e) => {
                                    // If an error occurs and we've accumulated some blocks,
                                    // yield an error value that carries those blocks.
                                    if !this.current_chunk.is_empty() {
                                        let chunk = std::mem::take(&mut this.current_chunk);
                                        this.accumulated_size = 0;
                                        return Poll::Ready(Some(Err(ChunkError {
                                            blocks: NonEmpty::from_vec(chunk),
                                            error: e,
                                        })));
                                    } else {
                                        // No blocks accumulated—yield the error immediately.
                                        this.finished = true;
                                        return Poll::Ready(Some(Err(ChunkError {
                                            blocks: None,
                                            error: e,
                                        })));
                                    }
                                }
                            }
                        }
                        Poll::Ready(None) => {
                            // Underlying stream is finished.
                            this.finished = true;
                            break;
                        }
                    }
                }

                if this.current_chunk.is_empty() {
                    // No blocks accumulated and stream finished.
                    return Poll::Ready(None);
                }

                // We've accumulated a chunk that meets at least one threshold or because of a leftover.
                let chunk = std::mem::take(&mut this.current_chunk);
                this.accumulated_size = 0;
                if let Some(non_empty) = NonEmpty::from_vec(chunk) {
                    return Poll::Ready(Some(Ok(non_empty)));
                }
                Poll::Ready(None) // This should not happen.
            }
        }
    }

    use ext::TryChunkBlocksExt;
    use futures::{StreamExt, TryStreamExt};
    use tracing::info;

    use crate::{
        types::{CompressedFuelBlock, NonEmpty},
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
                    .try_chunk_blocks(self.max_blocks, self.max_size)
                    .map(|res| match res {
                        Ok(blocks) => (Some(blocks), None),
                        Err(err) => (err.blocks, Some(err.error)),
                    });

                while let Some((blocks_until_potential_error, maybe_err)) =
                    block_stream.next().await
                {
                    if let Some(blocks) = blocks_until_potential_error {
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
