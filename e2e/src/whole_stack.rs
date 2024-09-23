use std::time::Duration;

use fuel::HttpClient;
use storage::{DbWithProcess, Postgres, PostgresProcess};
use url::Url;

use crate::{
    committer::{Committer, CommitterProcess},
    eth_node::{ContractArgs, DeployedContract, EthNode, EthNodeProcess},
    fuel_node::{FuelNode, FuelNodeProcess},
    kms::{Kms, KmsKey, KmsProcess},
};

pub enum FuelNodeType {
    Local(FuelNodeProcess),
    Testnet {
        url: Url,
        block_producer_addr: String,
    },
}

impl FuelNodeType {
    pub fn url(&self) -> Url {
        match self {
            FuelNodeType::Local(fuel_node) => fuel_node.url().clone(),
            FuelNodeType::Testnet { url, .. } => url.clone(),
        }
    }
    pub fn block_producer_addr(&self) -> String {
        match self {
            FuelNodeType::Local(fuel_node) => hex::encode(fuel_node.consensus_pub_key().hash()),
            FuelNodeType::Testnet {
                block_producer_addr,
                ..
            } => block_producer_addr.clone(),
        }
    }
    pub fn client(&self) -> HttpClient {
        match self {
            FuelNodeType::Local(fuel_node) => fuel_node.client(),
            FuelNodeType::Testnet { .. } => HttpClient::new(&self.url(), 10),
        }
    }
}

#[allow(dead_code)]
pub struct WholeStack {
    pub eth_node: EthNodeProcess,
    pub fuel_node: FuelNodeType,
    pub committer: CommitterProcess,
    pub db: DbWithProcess,
    pub deployed_contract: DeployedContract,
    pub contract_args: ContractArgs,
    pub kms: KmsProcess,
}

impl WholeStack {
    pub async fn deploy_default(logs: bool, blob_support: bool) -> anyhow::Result<Self> {
        let kms = start_kms(logs).await?;

        let eth_node = start_eth(logs).await?;
        let (main_key, secondary_key) = create_and_fund_kms_keys(&kms, &eth_node).await?;

        let (contract_args, deployed_contract) = deploy_contract(&eth_node, &main_key).await?;

        let fuel_node = FuelNodeType::Local(start_fuel_node(logs).await?);

        let db = start_db().await?;

        let committer = start_committer(
            true,
            blob_support,
            db.clone(),
            &eth_node,
            fuel_node.url(),
            fuel_node.block_producer_addr(),
            &deployed_contract,
            &main_key,
            &secondary_key,
        )
        .await?;

        Ok(WholeStack {
            eth_node,
            fuel_node,
            committer,
            db,
            deployed_contract,
            contract_args,
            kms,
        })
    }

    pub async fn connect_to_testnet(logs: bool, blob_support: bool) -> anyhow::Result<Self> {
        let kms = start_kms(logs).await?;

        let eth_node = start_eth(logs).await?;
        let (main_key, secondary_key) = create_and_fund_kms_keys(&kms, &eth_node).await?;

        let (contract_args, deployed_contract) = deploy_contract(&eth_node, &main_key).await?;

        let fuel_node = FuelNodeType::Testnet {
            url: "https://testnet.fuel.network/v1/graphql".parse().unwrap(),
            block_producer_addr: "d9173046b109cc24dfa1099d3c48d8b8b810e3279344cfc3d2bd13149e18c402"
                .to_owned(),
        };

        let db = start_db().await?;

        eprintln!("Starting committer");
        let committer = {
            let committer_builder = Committer::default()
                .with_show_logs(true)
                .with_eth_rpc((eth_node).ws_url().clone())
                .with_fuel_rpc(fuel_node.url())
                .with_db_port(db.port())
                .with_db_name(db.db_name())
                .with_state_contract_address(deployed_contract.address())
                .with_fuel_block_producer_addr(fuel_node.block_producer_addr())
                .with_main_key_arn(main_key.id.clone())
                .with_kms_url(main_key.url.clone())
                .with_bundle_accumulation_timeout("1000s".to_owned())
                .with_bundle_blocks_to_accumulate("3000".to_string())
                .with_bundle_optimization_timeout("10s".to_owned())
                .with_bundle_block_height_lookback("3000".to_owned())
                .with_bundle_compression_level("level6".to_owned());

            let committer = if blob_support {
                committer_builder.with_blob_key_arn(secondary_key.id.clone())
            } else {
                committer_builder
            };
            committer.start().await?
        };
        eprintln!("Committer started");

        Ok(WholeStack {
            eth_node,
            fuel_node,
            committer,
            db,
            deployed_contract,
            contract_args,
            kms,
        })
    }
}

async fn start_kms(logs: bool) -> anyhow::Result<KmsProcess> {
    Kms::default().with_show_logs(logs).start().await
}

async fn start_eth(logs: bool) -> anyhow::Result<EthNodeProcess> {
    EthNode::default().with_show_logs(logs).start().await
}

async fn create_and_fund_kms_keys(
    kms: &KmsProcess,
    eth_node: &EthNodeProcess,
) -> anyhow::Result<(KmsKey, KmsKey)> {
    let amount = alloy::primitives::utils::parse_ether("10")?;

    let create_and_fund = || async {
        let key = kms.create_key().await?;
        eth_node.fund(key.address(), amount).await?;
        anyhow::Result::<_>::Ok(key)
    };

    Ok((create_and_fund().await?, create_and_fund().await?))
}

async fn deploy_contract(
    eth_node: &EthNodeProcess,
    main_wallet_key: &KmsKey,
) -> anyhow::Result<(ContractArgs, DeployedContract)> {
    let contract_args = ContractArgs {
        finalize_duration: Duration::from_secs(1),
        blocks_per_interval: 10u32,
        cooldown_between_commits: Duration::from_secs(1),
    };

    let deployed_contract = eth_node
        .deploy_state_contract(main_wallet_key.clone(), contract_args)
        .await?;

    Ok((contract_args, deployed_contract))
}

async fn start_fuel_node(logs: bool) -> anyhow::Result<FuelNodeProcess> {
    FuelNode::default().with_show_logs(logs).start().await
}

async fn start_db() -> anyhow::Result<DbWithProcess> {
    storage::PostgresProcess::shared()
        .await?
        .create_random_db()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[allow(clippy::too_many_arguments)]
async fn start_committer(
    logs: bool,
    blob_support: bool,
    random_db: DbWithProcess,
    eth_node: &EthNodeProcess,
    fuel_node_url: Url,
    fuel_node_consensus_pub_key: String,
    deployed_contract: &DeployedContract,
    main_key: &KmsKey,
    secondary_key: &KmsKey,
) -> anyhow::Result<CommitterProcess> {
    let committer_builder = Committer::default()
        .with_show_logs(logs)
        .with_eth_rpc((eth_node).ws_url().clone())
        .with_fuel_rpc(fuel_node_url)
        .with_db_port(random_db.port())
        .with_db_name(random_db.db_name())
        .with_state_contract_address(deployed_contract.address())
        .with_fuel_block_producer_addr(fuel_node_consensus_pub_key)
        .with_main_key_arn(main_key.id.clone())
        .with_kms_url(main_key.url.clone())
        .with_bundle_accumulation_timeout("5s".to_owned())
        .with_bundle_blocks_to_accumulate("400".to_string())
        .with_bundle_optimization_timeout("1s".to_owned())
        .with_bundle_block_height_lookback("20000".to_owned())
        .with_bundle_compression_level("level6".to_owned());

    let committer = if blob_support {
        committer_builder.with_blob_key_arn(secondary_key.id.clone())
    } else {
        committer_builder
    };

    committer.start().await
}
