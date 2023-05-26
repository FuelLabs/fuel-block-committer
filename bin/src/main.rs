#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _config = fuel_block_committer_cli::parse();
    println!("{:?}", _config.ethereum_wallet_key);
    println!("{:?}", _config.ethereum_rpc);
    println!("{:?}", _config.fuel_graphql_endpoint);
    println!("{:?}", _config.state_contract_address);
    println!("{:?}", _config.commit_interval);
    Ok(())
}
