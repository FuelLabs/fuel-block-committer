# fuel-block-committer

The Fuel Block Committer is a standalone service dedicated to uploading Fuel Block metadata to Ethereum.

## Building

Building the project doesn't require any special steps beyond the standard Cargo build process.

```shell
cargo build
```

## Testing

Testing the committer requires the use of docker-compose to stand-up an e2e environment containing both a fuel-core and 
ganache container. Setting up the docker environment is automated via the `run_tests.sh` script.
The script will also handle any initial setup and contract deployments required for e2e testing.

Unit tests also require having the correct version of fuel-core installed. To avoid this requirement, enable the
`fuel-core-lib` feature on the `fuels-test-helpers` dev-dependency to ensure the correct version of fuel-core 
is selected.

```shell
./run_tests.sh
```

