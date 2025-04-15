# Eigenbundler Benchmarks

This package contains a benchmark for measuring the performance of the Eigenbundler component for a specific use case.

## Benchmark Case

The benchmark tests the following specific scenario:

- Total data size: 28 MB
- Block size: 4 MB (7 blocks in total)
- Target compression ratio: 2.2x (highly compressible data)
- Fragment size: 3.5 MB
- Maximum fragments per bundle: 12
- Compression level: Level6 (default)

This specific test case simulates a real-world scenario where we need to bundle a significant amount of data with good compression characteristics.

## Running the Benchmark

To run the benchmark:

```bash
cargo bench -p benchmarks
```

## Interpreting Results

The benchmark results show:
1. Execution time for the bundling operation
2. **Throughput in blocks per second** - This is the critical metric, as it tells you how many blocks can be processed per second
3. The actual compression ratio achieved (printed to console)

Since new blocks arrive at a rate of 1 per second in the production environment, the benchmark will help you determine if the bundling process can keep up with this rate. The bundle operation should show a throughput of more than 1 block per second to ensure it can handle the incoming data without falling behind.

The results will help you determine:
1. Whether the bundling process can keep up with the incoming block rate (1 block/second)
2. The actual performance in terms of blocks processed per second with Level6 compression
3. Whether the target compression ratio of 2.2x is actually achieved

The benchmark report is generated in HTML format and can be found in `target/criterion/eigenbundler_specific_case/`.

## Example Output

Here's an example of what the benchmark output might look like:

```
Compression ratio: 2.24x, Processing rate: 126.83 blocks/sec

eigenbundler_specific_case/28MB_data_7blocks/Level6
                        time:   [54.394 ms 56.490 ms 58.599 ms]
                        thrpt:  [119.45 elem/s 123.92 elem/s 128.69 elem/s]
```

This shows that with compression level 6, the bundler can process around 120-129 blocks per second, which is well above the requirement of 1 block per second, and achieves a compression ratio of 2.24x, which is very close to the target of 2.2x. 