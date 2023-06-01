use ethers::types::Chain;
use fuel_block_committer::adapters::ethereum_adapter::{EthereumAdapter, EthereumRPC};
use fuels::{
    tx::Bytes32,
    types::block::{Block as FuelBlock, Header as FuelBlockHeader},
};
use testcontainers::{core::WaitFor, *};

#[derive(Debug, Default)]
pub struct EthNode;

impl Image for EthNode {
    type Args = ();

    fn name(&self) -> String {
        "doit".to_owned()
    }

    fn tag(&self) -> String {
        "latest".to_owned()
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stdout(
            "FuelERC20Gateway_impl: 0x58Bf5211B9A7176aeAFa7129516c4D7C83696951",
        )]
    }

    fn expose_ports(&self) -> Vec<u16> {
        vec![8545]
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_can_run_hello_world() {
    let docker = clients::Cli::default();

    let container = docker.run(EthNode);
    let port = container.get_host_port_ipv4(8545);

    let ethereum_rpc = format!("http://127.0.0.1:{port}").parse().unwrap();
    let ethereum_chain_id = Chain::AnvilHardhat;
    let state_contract_address = "0xdAad669b06d79Cb48C8cfef789972436dBe6F24d"
        .parse()
        .unwrap();
    let ethereum_wallet_key = "0x9e56ccf010fa4073274b8177ccaad46fbaf286645310d03ac9bb6afa922a7c36";
    let eth_errors_before_unhealthy = 3;

    let ethereum_rpc = EthereumRPC::new(
        &ethereum_rpc,
        ethereum_chain_id,
        state_contract_address,
        ethereum_wallet_key,
        eth_errors_before_unhealthy,
    )
    .unwrap();

    let res = ethereum_rpc.submit(given_a_block(1)).await.unwrap();
    dbg!(&res);
}

fn given_a_block(block_height: u32) -> FuelBlock {
    let header = FuelBlockHeader {
        id: Bytes32::zeroed(),
        da_height: 0,
        transactions_count: 0,
        message_receipt_count: 0,
        transactions_root: Bytes32::zeroed(),
        message_receipt_root: Bytes32::zeroed(),
        height: block_height,
        prev_root: Bytes32::zeroed(),
        time: None,
        application_hash: Bytes32::zeroed(),
    };

    FuelBlock {
        id: Bytes32::default(),
        header,
        transactions: vec![],
    }
}
