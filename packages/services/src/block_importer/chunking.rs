use futures::stream::Stream;
use futures::task::{Context, Poll};
use std::pin::Pin;

use crate::types::{CompressedFuelBlock, NonEmpty};

/// The error type produced by our chunking combinator when an error is encountered.
/// It carries any blocks that were accumulated before the error, plus the error itself.
#[derive(Debug, PartialEq, Eq)]
pub struct ChunkError<E> {
    pub blocks: Option<NonEmpty<CompressedFuelBlock>>,
    pub error: E,
}

/// Extension trait that adds the `try_chunk_blocks` method to streams that yield blocks.
pub trait TryChunkBlocksExt<E>: Stream<Item = Result<CompressedFuelBlock, E>> + Sized {
    /// Returns a stream that groups blocks into chunks based on the provided thresholds.
    ///
    /// - `num_blocks` is the maximum number of blocks per chunk.
    /// - `max_size` is the maximum cumulative size (in bytes) per chunk.
    ///
    /// If an error occurs during accumulation, the stream yields an error value carrying
    /// any accumulated blocks, then terminates.
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

/// A stream combinator that accumulates blocks into larger chunks.
pub struct TryChunkBlocks<S> {
    stream: S,
    num_blocks: usize,
    max_size: usize,
    /// A block that did not “fit” in the current chunk.
    leftover: Option<CompressedFuelBlock>,
    /// Accumulated blocks for the current chunk.
    current_chunk: Vec<CompressedFuelBlock>,
    /// Total size (in bytes) of the accumulated blocks.
    accumulated_size: usize,
    /// Indicates that the underlying stream is finished (or terminated due to error).
    finished: bool,
}

impl<S> TryChunkBlocks<S> {
    /// Create a new instance of the chunking stream.
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

    /// Adds a block to the current chunk and updates the accumulated size.
    fn add_block(&mut self, block: CompressedFuelBlock) {
        self.accumulated_size += block.data.len();
        self.current_chunk.push(block);
    }

    /// Returns true if adding a block with `block_size` would exceed one of the thresholds.
    fn would_exceed(&self, block_size: usize) -> bool {
        // Only check thresholds if we already have items.
        !self.current_chunk.is_empty()
            && ((self.current_chunk.len() + 1) > self.num_blocks
                || (self.accumulated_size + block_size) > self.max_size)
    }

    /// Flushes the current chunk into a NonEmpty value, if nonempty.
    /// Resets the accumulator.
    fn flush_chunk(&mut self) -> Option<NonEmpty<CompressedFuelBlock>> {
        if self.current_chunk.is_empty() {
            None
        } else {
            let chunk = std::mem::take(&mut self.current_chunk);
            self.accumulated_size = 0;
            NonEmpty::from_vec(chunk)
        }
    }
}

impl<S> Unpin for TryChunkBlocks<S> {}

impl<S, E> Stream for TryChunkBlocks<S>
where
    S: Stream<Item = Result<CompressedFuelBlock, E>> + Unpin,
{
    type Item = Result<NonEmpty<CompressedFuelBlock>, ChunkError<E>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        // If we've already finished, flush any remaining accumulated chunk.
        if this.finished {
            return Poll::Ready(this.flush_chunk().map(Ok));
        }

        // If there is a leftover block from a previous poll, add it.
        if let Some(block) = this.leftover.take() {
            this.add_block(block);
        }

        // Accumulate blocks until a threshold is met or the underlying stream is exhausted.
        loop {
            // If we already have a chunk meeting a threshold, exit the loop.
            if this.would_exceed(0) {
                break;
            }

            match Pin::new(&mut this.stream).poll_next(cx) {
                Poll::Pending => {
                    // Don't yield partial chunk on Pending; wait for more.
                    return Poll::Pending;
                }
                Poll::Ready(Some(item)) => match item {
                    Ok(block) => {
                        let block_size = block.data.len();
                        if this.would_exceed(block_size) {
                            // Save the block for the next chunk and break.
                            this.leftover = Some(block);
                            break;
                        }
                        this.add_block(block);
                    }
                    Err(e) => {
                        // On error, if we've accumulated any blocks, yield them with the error.
                        if !this.current_chunk.is_empty() {
                            let chunk = std::mem::take(&mut this.current_chunk);
                            this.accumulated_size = 0;
                            // Mark finished so no further items are polled.
                            this.finished = true;
                            return Poll::Ready(Some(Err(ChunkError {
                                blocks: NonEmpty::from_vec(chunk),
                                error: e,
                            })));
                        } else {
                            // No blocks accumulated—yield error immediately and finish.
                            this.finished = true;
                            return Poll::Ready(Some(Err(ChunkError {
                                blocks: None,
                                error: e,
                            })));
                        }
                    }
                },
                Poll::Ready(None) => {
                    // Underlying stream exhausted.
                    this.finished = true;
                    break;
                }
            }
        }

        // Flush the accumulated chunk if any.
        if let Some(chunk) = this.flush_chunk() {
            Poll::Ready(Some(Ok(chunk)))
        } else {
            Poll::Ready(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;
    use futures::StreamExt;
    use itertools::Itertools;
    use nonempty::NonEmpty;

    use crate::block_importer::chunking::{ChunkError, TryChunkBlocksExt};
    use crate::types::CompressedFuelBlock;
    use crate::Result;

    /// Helper to generate a block with the given height and a data vector of the given size.
    fn gen_block(height: u32, size: usize) -> CompressedFuelBlock {
        // Here we assume that NonEmpty::from_vec never fails for nonempty vectors.
        CompressedFuelBlock {
            height,
            data: NonEmpty::from_vec(vec![0u8; size]).unwrap(),
        }
    }

    #[tokio::test]
    async fn test_chunk_by_count() {
        // Create a stream of 10 blocks (each with 1 byte) so that only the count threshold is relevant.
        let blocks: Vec<_> = (0..10).map(|i| Result::Ok(gen_block(i, 1))).collect();
        let s = stream::iter(blocks);
        // Set num_blocks=3 and a high max_size.
        let mut chunked = s.try_chunk_blocks(3, 1000);
        let mut sizes = Vec::new();

        while let Some(item) = chunked.next().await {
            match item {
                Ok(ne) => sizes.push(ne.len()),
                Err(err) => panic!("Unexpected error: {:?}", err),
            }
        }
        // Expect chunks of sizes: 3, 3, 3, and then 1 (because 3+3+3+1 = 10).
        assert_eq!(sizes, vec![3, 3, 3, 1]);
    }

    #[tokio::test]
    async fn test_chunk_by_size() {
        // Create a stream of 10 blocks, each 5 bytes in size.
        let blocks: Vec<_> = (0..10).map(|i| Result::Ok(gen_block(i, 5))).collect();
        let s = stream::iter(blocks);
        // Set a high num_blocks and max_size=10, so each chunk can only hold 2 blocks.
        let mut chunked = s.try_chunk_blocks(100, 10);
        let mut sizes = Vec::new();

        while let Some(item) = chunked.next().await {
            match item {
                Ok(ne) => sizes.push(ne.len()),
                Err(err) => panic!("Unexpected error: {:?}", err),
            }
        }
        // Expect each chunk to contain exactly 2 blocks (2 * 5 = 10 bytes total),
        // so with 10 blocks we expect 5 chunks.
        assert_eq!(sizes, vec![2, 2, 2, 2, 2]);
    }

    #[tokio::test]
    async fn test_error_with_accumulated_blocks() {
        // Create a stream that yields 3 blocks, then an error.
        let mut items: Vec<Result<CompressedFuelBlock>> =
            (0..3).map(|i| Ok(gen_block(i, 1))).collect();
        items.push(Err(crate::Error::Other("error".to_string())));
        // Further items (which should not be consumed).
        items.extend((4..10).map(|i| Ok(gen_block(i, 1))));
        let s = stream::iter(items);
        // Set thresholds high enough so that the error occurs before reaching any limit.
        let mut chunked = s.try_chunk_blocks(10, 100);
        // Expect an error with the 3 accumulated blocks.
        if let Some(item) = chunked.next().await {
            match item {
                Err(ChunkError { blocks, error }) => {
                    let count = blocks.map(|ne| ne.len()).unwrap_or(0);
                    assert_eq!(count, 3);
                    assert_eq!(error, crate::Error::Other("error".to_string()));
                }
                Ok(_) => panic!("Expected error with partial blocks"),
            }
        } else {
            panic!("Expected an item");
        }
        // After yielding the error, the stream should now be finished.
        assert!(chunked.next().await.is_none());
    }

    #[tokio::test]
    async fn test_error_without_accumulated_blocks() {
        // Create a stream that yields an error immediately.
        let s = stream::iter(vec![Err("immediate error")]);
        let mut chunked = s.try_chunk_blocks(10, 100);
        if let Some(item) = chunked.next().await {
            match item {
                Err(ChunkError { blocks, error }) => {
                    assert!(blocks.is_none());
                    assert_eq!(error, "immediate error");
                }
                Ok(_) => panic!("Expected immediate error"),
            }
        } else {
            panic!("Expected an item");
        }
        assert!(chunked.next().await.is_none());
    }

    #[tokio::test]
    async fn test_all_elements_consumed() {
        // Create a stream with 5 blocks and no errors.
        let blocks: Vec<_> = (0..5).map(|i| Result::Ok(gen_block(i, 1))).collect();
        let num_blocks = blocks.len();
        let s = stream::iter(blocks);
        // Set thresholds high enough so the entire stream accumulates into one chunk.
        let mut chunked = s.try_chunk_blocks(10, 100);
        let mut total = 0;
        while let Some(item) = chunked.next().await {
            match item {
                Ok(ne) => total += ne.len(),
                Err(err) => panic!("Unexpected error: {:?}", err),
            }
        }
        assert_eq!(total, num_blocks);
    }
}
