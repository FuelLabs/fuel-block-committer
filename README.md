# fuel-block-committer

The Fuel Block Committer is a standalone service dedicated to uploading Fuel Block metadata to Ethereum.

## Building

Building the project doesn't require any special steps beyond the standard Cargo build process.

```shell
cargo build
```

## Testing

To run the e2e tests you need to have the following installed and available in your PATH:
* [foundry](https://github.com/foundry-rs/foundry)
* fuel-core (can be installed via [fuelup](https://github.com/FuelLabs/fuelup))
* fuel-block-committer

You can also use `run_tests.sh` which takes care of building the `fuel-block-committer` binary and making it available on PATH prior to running the e2e tests.
