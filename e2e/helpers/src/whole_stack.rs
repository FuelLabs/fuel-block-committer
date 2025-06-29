use std::time::Duration;

use alloy::network::TxSigner;
use eth::L1Signers;
use fuel::HttpClient;
use signers::eth::kms::TestEthKmsSigner;
use storage::DbWithProcess;
use url::Url;

use crate::{
    committer::{Committer, CommitterProcess},
    eth_node::{ContractArgs, DeployedContract, EthNode, EthNodeProcess},
    fuel_node::{FuelNode, FuelNodeProcess},
    kms::{Kms, KmsProcess},
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
        let eth_signers = create_and_fund_kms_signers(&kms, &eth_node).await?;

        let request_timeout = Duration::from_secs(5);
        let max_fee = 1_000_000_000_000;

        let (contract_args, deployed_contract) =
            deploy_contract(&eth_node, eth_signers.clone(), max_fee, request_timeout).await?;

        let fuel_node = FuelNodeType::Local(start_fuel_node(logs, None).await?);

        let db = start_db().await?;

        let committer = start_committer(
            true,
            blob_support,
            db.clone(),
            &eth_node,
            &fuel_node.url(),
            &deployed_contract,
            eth_signers,
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
}

pub async fn start_kms(logs: bool) -> anyhow::Result<KmsProcess> {
    Kms::default().with_show_logs(logs).start().await
}

pub async fn start_eth(logs: bool) -> anyhow::Result<EthNodeProcess> {
    EthNode::default().with_show_logs(logs).start().await
}

pub async fn create_and_fund_kms_signers(
    kms: &KmsProcess,
    eth_node: &EthNodeProcess,
) -> anyhow::Result<L1Signers<TestEthKmsSigner, TestEthKmsSigner>> {
    let amount = alloy::primitives::utils::parse_ether("10")?;

    let create_and_fund = || async {
        let key = kms.create_key().await?;
        let signer = kms.eth_signer(key.id.clone()).await?;

        eth_node.fund(signer.address(), amount).await?;
        anyhow::Result::<_>::Ok(signer)
    };

    let main = create_and_fund().await?;
    let blob = create_and_fund().await?;
    let l1_signers = L1Signers {
        main,
        blob: Some(blob),
    };

    Ok(l1_signers)
}

pub async fn deploy_contract(
    eth_node: &EthNodeProcess,
    signers: L1Signers<TestEthKmsSigner, TestEthKmsSigner>,
    tx_max_fee: u128,
    request_timeout: Duration,
) -> anyhow::Result<(ContractArgs, DeployedContract)> {
    let contract_args = ContractArgs {
        finalize_duration: Duration::from_secs(1),
        blocks_per_interval: 10u32,
        cooldown_between_commits: Duration::from_secs(1),
    };

    let deployed_contract = eth_node
        .deploy_state_contract(signers, contract_args, tx_max_fee, request_timeout)
        .await?;

    Ok((contract_args, deployed_contract))
}

pub async fn start_fuel_node(
    logs: bool,
    poa_interval_period: Option<Duration>,
) -> anyhow::Result<FuelNodeProcess> {
    FuelNode::default()
        .with_show_logs(logs)
        .with_poa_interval_period(poa_interval_period)
        .start()
        .await
}

pub async fn start_db() -> anyhow::Result<DbWithProcess> {
    storage::PostgresProcess::shared()
        .await?
        .create_random_db()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[allow(clippy::too_many_arguments)]
pub async fn start_committer(
    logs: bool,
    blob_support: bool,
    random_db: DbWithProcess,
    eth_node: &EthNodeProcess,
    fuel_node_url: &Url,
    deployed_contract: &DeployedContract,
    eth_signers: L1Signers<TestEthKmsSigner, TestEthKmsSigner>,
) -> anyhow::Result<CommitterProcess> {
    let committer_builder = Committer::default()
        .with_show_logs(logs)
        .with_eth_rpc((eth_node).ws_url().clone())
        .with_fuel_rpc(fuel_node_url.clone())
        .with_db_port(random_db.port())
        .with_db_name(random_db.db_name())
        .with_state_contract_address(deployed_contract.address())
        .with_main_key_arn(eth_signers.main.key_id.clone())
        .with_kms_url(eth_signers.main.url.clone())
        .with_bundle_accumulation_timeout("5s".to_owned())
        .with_bundle_optimization_timeout("5s".to_owned())
        .with_block_bytes_to_accumulate("3 MB".to_string())
        .with_bundle_block_height_lookback("20000".to_owned())
        .with_bundle_fragments_to_accumulate("3".to_owned())
        .with_bundle_fragment_accumulation_timeout("5s".to_owned())
        .with_bundle_optimization_step("100".to_owned())
        .with_bundle_compression_level("level6".to_owned())
        .with_new_bundle_check_interval("3s".to_owned())
        .with_state_pruner_retention("10s".to_owned())
        .with_state_pruner_run_interval("20s".to_owned())
        .with_da_fee_check_interval("30s".to_owned())
        .with_da_layer_polling_interval("2s".to_owned())
        .with_da_layer_api_throughput(16777216);

    let committer = if blob_support {
        committer_builder.with_blob_key_arn(
            eth_signers
                .blob
                .expect("expected blob signer to be present")
                .key_id
                .clone(),
        )
    } else {
        committer_builder
    };

    committer.start().await
}

#[allow(clippy::too_many_arguments)]
pub async fn start_eigen_committer(
    logs: bool,
    random_db: DbWithProcess,
    eth_node: &EthNodeProcess,
    fuel_node_url: &Url,
    deployed_contract: &DeployedContract,
    main_eth_signer: TestEthKmsSigner,
    eigen_key: String,
    bytes_to_accumulate: &str,
) -> anyhow::Result<CommitterProcess> {
    let committer_builder = Committer::default()
        .with_show_logs(logs)
        .with_eth_rpc((eth_node).ws_url())
        .with_fuel_rpc(fuel_node_url.to_owned())
        .with_db_port(random_db.port())
        .with_db_name(random_db.db_name())
        .with_state_contract_address(deployed_contract.address())
        .with_main_key_arn(main_eth_signer.key_id.clone())
        .with_kms_url(main_eth_signer.url.clone())
        .with_bundle_accumulation_timeout("3600s".to_owned())
        .with_block_bytes_to_accumulate(bytes_to_accumulate.to_string())
        .with_bundle_optimization_timeout("60s".to_owned())
        .with_bundle_block_height_lookback("8500".to_owned())
        .with_bundle_compression_level("level4".to_owned())
        .with_bundle_optimization_step("100".to_owned())
        .with_bundle_fragments_to_accumulate("3".to_owned())
        .with_bundle_fragment_accumulation_timeout("10m".to_owned())
        .with_new_bundle_check_interval("3s".to_owned())
        .with_state_pruner_retention("1s".to_owned())
        .with_state_pruner_run_interval("30s".to_owned())
        .with_alt_da_key(eigen_key)
        .with_da_fee_check_interval("30s".to_owned())
        .with_da_layer_polling_interval("2s".to_owned())
        .with_da_layer_api_throughput(16777216);

    committer_builder.start().await
}
