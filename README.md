# fuel-block-committer

The Fuel Block Committer is a standalone service dedicated to uploading Fuel Block metadata to Ethereum.

## Building

Building the project doesn't require any special steps beyond the standard Cargo build process.

```shell
cargo build
```

## Testing

To run the end-to-end (e2e) tests, you need to have the following installed and available in your `PATH`:

- [Foundry](https://github.com/foundry-rs/foundry)
- [fuel-core](https://github.com/FuelLabs/fuelup) (can be installed via [fuelup](https://github.com/FuelLabs/fuelup))
- fuel-block-committer

You can also use `run_tests.sh`, which takes care of building the `fuel-block-committer` binary and making it available on `PATH` prior to running the e2e tests.

```shell
./run_tests.sh
```

## Schema Visualization

We use [SchemaSpy](https://github.com/schemaspy/schemaspy) to generate visual representations of the database schema in both `.dot` and `.png` formats.

### Generating Schema Diagrams

The CI pipeline is configured to automatically run the `update_db_preview.sh` script and check for any updates to the `db_preview` directory. If there are changes (e.g., new or modified `.dot` files), the CI step will fail, prompting you to commit these updates. This ensures that the repository remains in sync with the latest database schema visualizations.

#### Prerequisites

Before running the script, ensure you have the following installed:

- [Docker](https://www.docker.com/get-started) (required for running SchemaSpy)
- [Sqlx CLI](https://github.com/launchbadge/sqlx/tree/main/sqlx-cli) (required for running migrations)
