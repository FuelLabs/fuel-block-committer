#[cfg(test)]
mod committer;
#[cfg(test)]
mod eth_node;
#[cfg(test)]
mod fuel_node;

#[cfg(test)]
mod tests {
    use anyhow::{anyhow, Result};
    use eth::{Chain, WebsocketClient};
    use ports::fuel::{Api, FuelPublicKey};
    use std::time::Duration;
    use validator::{BlockValidator, Validator};

    use crate::committer::Committer;
    use crate::eth_node::EthNode;
    use crate::fuel_node::FuelNode;

    #[tokio::test(flavor = "multi_thread")]
    async fn submitted_correct_block_and_was_finalized() -> Result<()> {
        let finalize_duration = Duration::from_secs(1);

        let blocks_per_interval = 10u32;
        let commit_cooldown_seconds = 1u32;

        let eth_node = EthNode::default().with_show_logs(false).start().await?;
        let chain_state_contract_addr = eth_node
            .deploy_chain_state_contract(
                finalize_duration.as_secs(),
                blocks_per_interval,
                commit_cooldown_seconds,
            )
            .await?;

        let chain_state_contract = WebsocketClient::connect(
            &eth_node.ws_url(),
            Chain::AnvilHardhat,
            chain_state_contract_addr,
            &eth_node.wallet_key(),
            5,
        )
        .await?;

        let fuel_node = FuelNode::default().with_show_logs(false).start().await?;

        let db_process = storage::PostgresProcess::shared().await?;
        let random_db = db_process.create_random_db().await?;

        let db_name = random_db.db_name();
        let db_port = random_db.port();

        let fuel_block_producer_public_key = "0x73dc6cc8cc0041e4924954b35a71a22ccb520664c522198a6d31dc6c945347bb854a39382d296ec64c70d7cea1db75601595e29729f3fbdc7ee9dae66705beb4";

        let committer = Committer::default()
            .with_show_logs(false)
            .with_eth_rpc(eth_node.ws_url().clone())
            .with_fuel_rpc(fuel_node.url().clone())
            .with_db_port(db_port)
            .with_db_name(db_name)
            .with_state_contract_address(chain_state_contract_addr)
            .with_fuel_block_producer_public_key(fuel_block_producer_public_key.to_owned())
            .with_wallet_key(eth_node.wallet_key())
            .start()
            .await?;

        let fuel_client = fuel_node.client();

        fuel_client.produce_blocks(blocks_per_interval).await?;

        committer
            .wait_for_committed_block(blocks_per_interval as u64)
            .await?;
        let committed_at = tokio::time::Instant::now();

        tokio::time::sleep_until(committed_at + finalize_duration).await;

        let latest_block = fuel_client.latest_block().await?;

        // Had to use `serde_json` because `FromStr` did not work because of an validation error
        let producer_public_key: FuelPublicKey =
            serde_json::from_str(&format!("\"{fuel_block_producer_public_key}\""))
                .map_err(|e| anyhow!("could not parse producer pub key: {e:?}"))?;

        let validated_block = BlockValidator::new(producer_public_key).validate(&latest_block)?;

        assert!(chain_state_contract.finalized(validated_block).await?);

        Ok(())
    }
}
