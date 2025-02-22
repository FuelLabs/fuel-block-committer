use std::time::Duration;

use clap::Parser;
use e2e_helpers::whole_stack::{
    create_and_fund_kms_keys, deploy_contract, start_committer, start_db, start_eth,
    start_fuel_node, start_kms, FuelNodeType,
};
use fuel::{HttpClient, Url};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long, default_value_t = 128)]
    block_size: usize,

    #[arg(long, default_value_t = 4000)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // if std::env::var("RUST_LOG").is_err() {
    //     std::env::set_var("RUST_LOG", "info");
    // }

    // env_logger::init();
    let args = Args::parse();

    let mut fuel_node = e2e_helpers::fuel_node_simulated::FuelNode::new(args.port);
    fuel_node.run(args.block_size);
    // let client = HttpClient::new(&fuel_node.url(), 100, 1.try_into().unwrap());

    let logs = false;

    let kms = start_kms(logs).await?;

    let eth_node = start_eth(logs).await?;
    let (main_key, secondary_key) = create_and_fund_kms_keys(&kms, &eth_node).await?;

    let request_timeout = Duration::from_secs(50);
    let max_fee = 1_000_000_000_000;

    let (_contract_args, deployed_contract) =
        deploy_contract(&eth_node, &main_key, max_fee, request_timeout).await?;

    let db = start_db().await?;

    let _committer = start_committer(
        true,
        true,
        db.clone(),
        &eth_node,
        fuel_node.url(),
        &deployed_contract,
        &main_key,
        &secondary_key,
    )
    .await?;

    tokio::time::sleep(Duration::from_secs(300)).await;

    Ok(())
}
