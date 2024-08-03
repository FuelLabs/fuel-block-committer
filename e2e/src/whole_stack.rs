use std::{sync::Arc, time::Duration};

use storage::PostgresProcess;

use crate::{
    committer::{Committer, CommitterProcess},
    eth_node::{ContractArgs, DeployedContract, EthNode, EthNodeProcess},
    fuel_node::{FuelNode, FuelNodeProcess},
    kms::{Kms, KmsProcess},
};

#[allow(dead_code)]
pub struct WholeStack {
    pub eth_node: EthNodeProcess,
    pub fuel_node: FuelNodeProcess,
    pub committer: CommitterProcess,
    pub db: Arc<PostgresProcess>,
    pub deployed_contract: DeployedContract,
    pub contract_args: ContractArgs,
    pub kms: KmsProcess,
}

impl WholeStack {
    pub async fn deploy_default(logs: bool, blob_support: bool) -> anyhow::Result<Self> {
        let kms = Kms::default().with_show_logs(logs).start().await?;
        let eth_node = EthNode::default().with_show_logs(logs).start().await?;

        let amount = ethers::utils::parse_ether("400")?;

        let main_wallet_key = kms.create_key(eth_node.chain_id()).await?;
        eth_node.fund(main_wallet_key.address(), amount).await?;

        let contract_args = ContractArgs {
            finalize_duration: Duration::from_secs(1),
            blocks_per_interval: 10u32,
            cooldown_between_commits: Duration::from_secs(1),
        };

        let deployed_contract = eth_node
            .deploy_state_contract(main_wallet_key.clone(), contract_args)
            .await?;

        let secondary_wallet_key = kms.create_key(eth_node.chain_id()).await?;
        eth_node
            .fund(secondary_wallet_key.address(), amount)
            .await?;

        let fuel_node = FuelNode::default().with_show_logs(logs).start().await?;

        let db_process = storage::PostgresProcess::shared().await?;
        let random_db = db_process.create_random_db().await?;

        let fuel_consensus_pub_key = fuel_node.consensus_pub_key();

        let db_name = random_db.db_name();
        let db_port = random_db.port();
        let committer_builder = Committer::default()
            .with_show_logs(logs)
            .with_eth_rpc(eth_node.ws_url().clone())
            .with_fuel_rpc(fuel_node.url().clone())
            .with_db_port(db_port)
            .with_db_name(db_name)
            .with_state_contract_address(deployed_contract.address())
            .with_fuel_block_producer_public_key(fuel_consensus_pub_key)
            .with_main_key_id(main_wallet_key.id)
            .with_aws_region(kms.region().clone());

        let committer = if blob_support {
            committer_builder.with_blob_key_id(secondary_wallet_key.id)
        } else {
            committer_builder
        };

        let committer = committer.start().await?;

        Ok(Self {
            eth_node,
            fuel_node,
            committer,
            deployed_contract,
            db: db_process,
            kms,
            contract_args,
        })
    }
}
