use alloy::network::TxSigner;
use anyhow::Result;
use std::time::Duration;
use tokio::time::sleep;
use url::Url;

use e2e_helpers::{
    committer::Committer,
    eth_node::{ContractArgs, DeployedContract, EthNode, EthNodeProcess},
    kms::{Kms, KmsKey, KmsProcess},
    whole_stack::{create_and_fund_kms_signers, deploy_contract},
};

#[tokio::main]
async fn main() -> Result<()> {
    let kms = start_kms(false).await?;

    let eth_node = start_eth(false).await?;
    let eth_signers = create_and_fund_kms_signers(&kms, &eth_node).await?;
    let eigen_key = "".to_string(); // TODO: fill in eigen_key

    let request_timeout = Duration::from_secs(5);
    let max_fee = 1_000_000_000_000;

    let (contract_args, deployed_contract) =
        deploy_contract(&eth_node, eth_signers.clone(), max_fee, request_timeout).await?;

    let db = start_db().await?;

    let fuel_node_url = Url::parse("http://localhost:4000").unwrap();

    let committer = {
        let committer_builder = Committer::default()
            .with_show_logs(true)
            .with_eth_rpc((eth_node).ws_url())
            .with_fuel_rpc(fuel_node_url)
            .with_db_port(db.port())
            .with_db_name(db.db_name())
            .with_state_contract_address(deployed_contract.address())
            .with_main_key_arn(eth_signers.main.key_id)
            .with_kms_url(eth_signers.main.url)
            .with_bundle_accumulation_timeout("3600s".to_owned())
            .with_block_bytes_to_accumulate("3 MB".to_string())
            .with_bundle_optimization_timeout("60s".to_owned())
            .with_bundle_block_height_lookback("8500".to_owned())
            .with_bundle_optimization_step("100".to_owned())
            .with_bundle_fragments_to_accumulate("3".to_owned())
            .with_bundle_fragment_accumulation_timeout("10m".to_owned())
            .with_new_bundle_check_interval("3s".to_owned())
            .with_state_pruner_retention("1s".to_owned())
            .with_state_pruner_run_interval("30s".to_owned())
            .with_alt_da_key(eigen_key);

        committer_builder.start().await?
    };

    println!("Setup complete!");
    println!("Ethereum node WS URL: {}", eth_node.ws_url());
    println!("Contract address: {}", deployed_contract.address());
    println!("DB Port: {}", db.port());
    println!("DB Name: {}", db.db_name());

    // keep the process running
    loop {
        sleep(Duration::from_secs(1)).await;
    }
}

async fn start_kms(logs: bool) -> Result<KmsProcess> {
    Kms::default().with_show_logs(logs).start().await
}

async fn start_eth(logs: bool) -> Result<EthNodeProcess> {
    EthNode::default().with_show_logs(logs).start().await
}

async fn start_db() -> Result<storage::DbWithProcess> {
    storage::PostgresProcess::shared()
        .await?
        .create_random_db()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}
