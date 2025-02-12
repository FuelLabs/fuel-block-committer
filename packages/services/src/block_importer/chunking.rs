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

/// Extension trait that adds the `try_chunk_blocks` method.
pub trait TryChunkBlocksExt<E>: Stream<Item = Result<CompressedFuelBlock, E>> + Sized {
    /// Chunk the stream so that each yielded item is either a full chunk of blocks
    /// (i.e. up to `num_blocks` items or accumulating at most `max_size` bytes)
    /// or (if an error occurs) an error value carrying any blocks accumulated so far.
    ///
    /// In this version we do not yield a partial chunk merely because the underlying
    /// stream returns Pending—we wait to accumulate more blocks.
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
    // Each item is either a successful chunk or an error (with a partial chunk)
    type Item = Result<NonEmpty<CompressedFuelBlock>, ChunkError<E>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        // If finished, yield any final accumulated chunk and then None.
        if this.finished {
            if !this.current_chunk.is_empty() {
                let chunk = std::mem::take(&mut this.current_chunk);
                this.accumulated_size = 0;
                if let Some(non_empty) = NonEmpty::from_vec(chunk) {
                    return Poll::Ready(Some(Ok(non_empty)));
                }
            }
            return Poll::Ready(None);
        }

        // Add a leftover block from a previous poll if present.
        if let Some(block) = this.leftover.take() {
            this.accumulated_size += block.data.len();
            this.current_chunk.push(block);
        }

        // Try to pull items from the underlying stream until a threshold is met.
        loop {
            // Break if we already have a nonempty chunk that meets a threshold.
            if !this.current_chunk.is_empty()
                && (this.current_chunk.len() >= this.num_blocks
                    || this.accumulated_size >= this.max_size)
            {
                break;
            }

            match Pin::new(&mut this.stream).poll_next(cx) {
                Poll::Pending => {
                    // Instead of yielding a partial chunk when Pending,
                    // return Pending to allow further accumulation.
                    return Poll::Pending;
                }
                Poll::Ready(Some(item)) => match item {
                    Ok(block) => {
                        let block_size = block.data.len();
                        // If adding this block would exceed a threshold (and we already have items),
                        // save it as leftover and break.
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
                        // If an error occurs and we have accumulated some blocks, yield them with the error.
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
                },
                Poll::Ready(None) => {
                    // Underlying stream finished.
                    this.finished = true;
                    break;
                }
            }
        }

        if this.current_chunk.is_empty() {
            return Poll::Ready(None);
        }

        // Yield the accumulated chunk.
        let chunk = std::mem::take(&mut this.current_chunk);
        this.accumulated_size = 0;
        if let Some(non_empty) = NonEmpty::from_vec(chunk) {
            return Poll::Ready(Some(Ok(non_empty)));
        }
        Poll::Ready(None) // This case should not occur.
    }
}
#[cfg(test)]
mod tests {
    // tests/stream_ext_tests.rs
    use futures::stream;
    use futures::StreamExt;
    use nonempty::NonEmpty;

    use crate::block_importer::chunking::ChunkError;
    use crate::block_importer::chunking::TryChunkBlocksExt;
    use crate::types::CompressedFuelBlock;
    use crate::Result;

    // A helper to generate a block with the given height and data size.
    fn gen_block(height: u32, size: usize) -> CompressedFuelBlock {
        CompressedFuelBlock {
            height,
            data: NonEmpty::from_vec(vec![0u8; size]).unwrap(),
        }
    }

    #[tokio::test]
    async fn test_chunk_by_count() {
        // Create a stream of 10 blocks (each with size 1 byte) so that the size threshold is not reached.
        let blocks: Vec<_> = (0..10).map(|i| Result::Ok(gen_block(i, 1))).collect();
        let s = stream::iter(blocks);
        // Set num_blocks=3 and a high max_size.
        let mut chunked = s.try_chunk_blocks(3, 100);
        let mut sizes = Vec::new();

        while let Some(item) = chunked.next().await {
            match item {
                Ok(ne) => sizes.push(ne.len()),
                Err(err) => panic!("Unexpected error: {:?}", err),
            }
        }
        // Expect chunks of sizes: 3, 3, 3, and then 1 (since 3+3+3+1 = 10).
        assert_eq!(sizes, vec![3, 3, 3, 1]);
    }

    #[tokio::test]
    async fn test_chunk_by_size() {
        // Create a stream of 10 blocks, each of size 5 bytes.
        let blocks: Vec<_> = (0..10).map(|i| Result::Ok(gen_block(i, 5))).collect();
        let s = stream::iter(blocks);
        // Set max_size=10 (so that each chunk can hold 2 blocks of 5 bytes) and a high num_blocks.
        let mut chunked = s.try_chunk_blocks(100, 10);
        let mut sizes = Vec::new();

        while let Some(item) = chunked.next().await {
            match item {
                Ok(ne) => sizes.push(ne.len()),
                Err(err) => panic!("Unexpected error: {:?}", err),
            }
        }
        // Expect each chunk to contain exactly 2 blocks (10 bytes total), and 10 blocks in total.
        assert_eq!(sizes, vec![2, 2, 2, 2, 2]);
    }

    #[tokio::test]
    async fn test_error_with_accumulated_blocks() {
        // Create a stream that yields 3 blocks then an error.
        let mut items: Vec<Result<CompressedFuelBlock>> =
            (0..3).map(|i| Ok(gen_block(i, 1))).collect();
        items.push(Err(crate::Error::Other("error".to_string())));
        // Further items (which should not be consumed).
        items.extend((4..10).map(|i| Ok(gen_block(i, 1))));
        let s = stream::iter(items);
        // Set thresholds high enough so that count is not reached before the error.
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
        // Stream should now be finished.
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
        // Set thresholds so that the entire stream is accumulated into one chunk.
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
