use clap::{command, Parser};
use fuel_types::{Bytes20, Bytes32};
use url::Url;

const ETHEREUM_RPC: &str = "https://mainnet.infura.io/v3/YOUR_PROJECT_ID";
const FUEL_GRAPHQL_ENDPOINT: &str = "https://127.0.0.1:4000";
const STATE_CONTRACT_ADDRESS: Bytes20 = Bytes20::zeroed();
const COMMIT_INTERVAL: u32 = 1;

#[derive(Debug, Clone)]
pub struct Config {
    pub ethereum_wallet_key: Bytes32,
    pub ethereum_rpc: Url,
    pub fuel_graphql_endpoint: Url,
    pub state_contract_address: Bytes20,
    pub commit_interval: u32,
}

#[derive(Parser)]
#[command(
    name = "fuel-block-committer",
    version,
    about,
    propagate_version = true
)]
struct Cli {
    /// Ethereum wallet key
    #[arg(
        long,
        env = "ETHEREUM_WALLET_KEY",
        value_name = "BYTES32",
        help = "The secret key authorized by the L1 bridging contracts to post block commitments.",
    )]
    ethereum_wallet_key: Bytes32,

    /// Ethereum RPC
    #[arg(long,
    env = "ETHEREUM_RPC",
    default_value = ETHEREUM_RPC, value_name = "URL", help = "URL to a Ethereum RPC endpoint.")]
    ethereum_rpc: Url,

    /// Fuel GraphQL endpoint
    #[arg(long,
    env = "FUEL_GRAPHQL_ENDPOINT",
    default_value = FUEL_GRAPHQL_ENDPOINT, value_name = "URL", help = "URL to a fuel-core graphql endpoint.")]
    fuel_graphql_endpoint: Url,

    /// State contract address
    #[arg(long,
    env = "STATE_CONTRACT_ADDRESS",
    default_value_t = STATE_CONTRACT_ADDRESS, value_name = "BYTES20", help = "Ethereum address of the fuel chain state contract.")]
    state_contract_address: Bytes20,

    /// Commit interval
    #[arg(long,
    env = "COMMIT_INTERVAL",
    default_value_t = COMMIT_INTERVAL, value_name = "U32", help = "The number of fuel blocks between ethereum commits. If set to 1, then every block should be pushed to Ethereum.")]
    commit_interval: u32,
}

pub fn parse() -> anyhow::Result<Config> {
    let cli = Cli::try_parse()?;
    Ok(Config {
        ethereum_wallet_key: cli.ethereum_wallet_key,
        ethereum_rpc: cli.ethereum_rpc,
        fuel_graphql_endpoint: cli.fuel_graphql_endpoint,
        state_contract_address: cli.state_contract_address,
        commit_interval: cli.commit_interval,
    })
}

#[cfg(test)]
mod tests {
    use assert_cmd::prelude::*; // Add methods on commands
    use std::process::Command;

    #[test]
    fn test_invalid_inputs() -> Result<(), Box<dyn std::error::Error>> {
        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            let no_key_or_env_var = cmd.assert();
            assert!(
            String::from_utf8_lossy(&no_key_or_env_var.get_output().stderr)
                .starts_with("error: The following required argument was not provided: ethereum_wallet_key")
            );
        }

        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--ethereum-wallet-key");
            let key_val_missing = cmd.assert();
            assert!(
                String::from_utf8_lossy(&key_val_missing.get_output().stderr).starts_with(
                    "error: a value is required for '--ethereum-wallet-key <BYTES32>'"
                )
            );
            cmd.arg("0123456789");
            let bad_key_val = cmd.assert();
            assert!(
                String::from_utf8_lossy(&bad_key_val.get_output().stderr).starts_with(
                    "error: invalid value '0123456789' for '--ethereum-wallet-key <BYTES32>'"
                )
            );
        }

        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--ethereum-rpc");
            let rpc_missing = cmd.assert();
            assert!(
                String::from_utf8_lossy(&rpc_missing.get_output().stderr).starts_with(
                    "error: a value is required for '--ethereum-rpc <URL>' but none was supplied"
                )
            );
            cmd.arg("not valid url");
            let bad_rpc_value = cmd.assert();
            assert!(String::from_utf8_lossy(&bad_rpc_value.get_output().stderr)
                .starts_with("error: invalid value 'not valid url' for '--ethereum-rpc <URL>'"));
        }

        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--fuel-graphql-endpoint");
            let fge_missing = cmd.assert();
            assert!(
                String::from_utf8_lossy(&fge_missing.get_output().stderr).starts_with(
                    "error: a value is required for '--fuel-graphql-endpoint <URL>' but none was supplied"
                )
            );
            cmd.arg("not valid url");
            let bad_fge_value = cmd.assert();
            assert!(
                String::from_utf8_lossy(&bad_fge_value.get_output().stderr).starts_with(
                    "error: invalid value 'not valid url' for '--fuel-graphql-endpoint <URL>'"
                )
            );
        }

        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--state-contract-address");
            let sca_missing = cmd.assert();
            assert!(
                String::from_utf8_lossy(&sca_missing.get_output().stderr).starts_with(
                    "error: a value is required for '--state-contract-address <BYTES20>' but none was supplied"
                )
            );
            cmd.arg("123");
            let bad_sca_value = cmd.assert();
            assert!(
                String::from_utf8_lossy(&bad_sca_value.get_output().stderr).starts_with(
                    "error: invalid value '123' for '--state-contract-address <BYTES20>'"
                )
            );
        }

        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--commit-interval");
            let ci_missing = cmd.assert();
            assert!(
                String::from_utf8_lossy(&ci_missing.get_output().stderr).starts_with(
                    "error: a value is required for '--commit-interval <U32>' but none was supplied"
                )
            );
            cmd.arg("asd");
            let bad_ci_value = cmd.assert();
            assert!(String::from_utf8_lossy(&bad_ci_value.get_output().stderr)
                .starts_with("error: invalid value 'asd' for '--commit-interval <U32>'"));
        }

        Ok(())
    }

    #[test]
    fn test_valid_inputs() -> Result<(), Box<dyn std::error::Error>> {
        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--ethereum-wallet-key");
            cmd.arg("0123456789abcdeffedcba9876543210aabbccddeeff112233445566778899aa");
            cmd.assert().success();
        }

        {
            std::env::set_var(
                "ETHEREUM_WALLET_KEY",
                "0123456789abcdeffedcba9876543210aabbccddeeff112233445566778899bbb",
            );
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.assert().success();
        }

        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--ethereum-wallet-key");
            cmd.arg("0123456789abcdeffedcba9876543210aabbccddeeff112233445566778899aa");
            cmd.arg("--ethereum-rpc");
            cmd.arg("https://mainnet.infura.io/v3/testintest");
            cmd.assert().success();
        }

        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--ethereum-wallet-key");
            cmd.arg("0123456789abcdeffedcba9876543210aabbccddeeff112233445566778899aa");
            cmd.arg("--ethereum-rpc");
            cmd.arg("https://mainnet.infura.io/v3/testintest");
            cmd.arg("--fuel-graphql-endpoint");
            cmd.arg("https://127.0.0.1:50000");
            cmd.assert().success();
        }

        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--ethereum-wallet-key");
            cmd.arg("0123456789abcdeffedcba9876543210aabbccddeeff112233445566778899aa");
            cmd.arg("--ethereum-rpc");
            cmd.arg("https://mainnet.infura.io/v3/testintest");
            cmd.arg("--fuel-graphql-endpoint");
            cmd.arg("https://127.0.0.1:50000");
            cmd.assert().success();
        }
        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--ethereum-wallet-key");
            cmd.arg("0123456789abcdeffedcba9876543210aabbccddeeff112233445566778899aa");
            cmd.arg("--ethereum-rpc");
            cmd.arg("https://mainnet.infura.io/v3/testintest");
            cmd.arg("--fuel-graphql-endpoint");
            cmd.arg("https://127.0.0.1:50000");
            cmd.arg("--state-contract-address");
            cmd.arg("0000000000000000000000000000000000225883");
            cmd.assert().success();
        }
        {
            let mut cmd = Command::cargo_bin("fuel-block-committer-bin")?;
            cmd.arg("--ethereum-wallet-key");
            cmd.arg("0123456789abcdeffedcba9876543210aabbccddeeff112233445566778899aa");
            cmd.arg("--ethereum-rpc");
            cmd.arg("https://mainnet.infura.io/v3/testintest");
            cmd.arg("--fuel-graphql-endpoint");
            cmd.arg("https://127.0.0.1:50000");
            cmd.arg("--state-contract-address");
            cmd.arg("0000000000000000000000000000000000225883");
            cmd.arg("--commit-interval");
            cmd.arg("225883");
            cmd.assert().success();
        }
        Ok(())
    }
}
