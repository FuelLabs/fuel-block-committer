use tokio::process::Command;
pub fn run_committer(
    fuel_port: u16,
    eth_port: u16,
    commit_interval: usize,
) -> tokio::process::Child {
    let args = [
        "run",
        "--",
        &format!("--fuel-graphql-endpoint=http://127.0.0.1:{fuel_port}"),
        &format!("--ethereum-rpc=ws://127.0.0.1:{eth_port}"),
        "--ethereum-wallet-key=0x9e56ccf010fa4073274b8177ccaad46fbaf286645310d03ac9bb6afa922a7c36",
        "--state-contract-address=0xdAad669b06d79Cb48C8cfef789972436dBe6F24d",
        "--ethereum-chain=anvil",
        &format!("--commit-interval={commit_interval}"),
    ];

    Command::new(env!("CARGO"))
        .kill_on_drop(true)
        .args(&args)
        .spawn()
        .unwrap()
}
