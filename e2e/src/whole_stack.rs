use std::time::Duration;

use fuel::HttpClient;
use storage::DbWithProcess;
use url::Url;

use crate::{
    committer::{Committer, CommitterProcess},
    eth_node::{ContractArgs, DeployedContract, EthNode, EthNodeProcess},
    fuel_node::{FuelNode, FuelNodeProcess},
    kms::{Kms, KmsKey, KmsProcess},
};

pub enum FuelNodeType {
    Local(FuelNodeProcess),
    Testnet { url: Url },
}

impl FuelNodeType {
    pub fn url(&self) -> Url {
        match self {
            FuelNodeType::Local(fuel_node) => fuel_node.url().clone(),
            FuelNodeType::Testnet { url, .. } => url.clone(),
        }
    }
    pub fn client(&self) -> HttpClient {
        match self {
            FuelNodeType::Local(fuel_node) => fuel_node.client(),
            FuelNodeType::Testnet { .. } => HttpClient::new(&self.url(), 10, 5.try_into().unwrap()),
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

        let request_timeout = Duration::from_secs(5);
        let max_fee = 1_000_000_000_000;

        let (contract_args, deployed_contract) =
            deploy_contract(&eth_node, &main_key, max_fee, request_timeout).await?;

        let fuel_node = FuelNodeType::Local(start_fuel_node(logs).await?);

        let db = start_db().await?;

        let committer = start_committer(
            logs,
            blob_support,
            db.clone(),
            &eth_node,
            fuel_node.url(),
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

        let request_timeout = Duration::from_secs(5);
        // 0.004 ETH
        let max_fee = 4000000000000000;

        let (contract_args, deployed_contract) =
            deploy_contract(&eth_node, &main_key, max_fee, request_timeout).await?;

        let fuel_node = FuelNodeType::Testnet {
            url: "https://testnet.fuel.network/v1/graphql".parse().unwrap(),
        };

        let db = start_db().await?;

        let committer = {
            let committer_builder = Committer::default()
                .with_show_logs(true)
                .with_eth_rpc((eth_node).ws_url().clone())
                .with_fuel_rpc(fuel_node.url())
                .with_db_port(db.port())
                .with_db_name(db.db_name())
                .with_state_contract_address(deployed_contract.address())
                .with_main_key_arn(main_key.id.clone())
                .with_kms_url(main_key.url.clone())
                .with_bundle_accumulation_timeout("3600s".to_owned())
                .with_bundle_blocks_to_accumulate("400".to_string())
                .with_bundle_optimization_timeout("60s".to_owned())
                .with_bundle_block_height_lookback("8500".to_owned())
                .with_bundle_optimization_step("100".to_owned())
                .with_bundle_fragments_to_accumulate("3".to_owned())
                .with_bundle_fragment_accumulation_timeout("10m".to_owned())
                .with_new_bundle_check_interval("3s".to_owned())
                .with_bundle_compression_level("level6".to_owned());

            let committer = if blob_support {
                committer_builder.with_blob_key_arn(secondary_key.id.clone())
            } else {
                committer_builder
            };
            committer.start().await?
        };

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
    tx_max_fee: u128,
    request_timeout: Duration,
) -> anyhow::Result<(ContractArgs, DeployedContract)> {
    let contract_args = ContractArgs {
        finalize_duration: Duration::from_secs(1),
        blocks_per_interval: 10u32,
        cooldown_between_commits: Duration::from_secs(1),
    };

    let deployed_contract = eth_node
        .deploy_state_contract(
            main_wallet_key.clone(),
            contract_args,
            tx_max_fee,
            request_timeout,
        )
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
        .with_main_key_arn(main_key.id.clone())
        .with_kms_url(main_key.url.clone())
        .with_bundle_accumulation_timeout("5s".to_owned())
        .with_bundle_blocks_to_accumulate("3600".to_string())
        .with_bundle_optimization_timeout("5s".to_owned())
        .with_bundle_block_height_lookback("20000".to_owned())
        .with_bundle_fragments_to_accumulate("3".to_owned())
        .with_bundle_fragment_accumulation_timeout("5s".to_owned())
        .with_bundle_optimization_step("100".to_owned())
        .with_bundle_compression_level("level6".to_owned())
        .with_new_bundle_check_interval("3s".to_owned());

    let committer = if blob_support {
        committer_builder.with_blob_key_arn(secondary_key.id.clone())
    } else {
        committer_builder
    };

    committer.start().await
}
