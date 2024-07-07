#[cfg(test)]
mod committer;
#[cfg(test)]
mod eth_node;
#[cfg(test)]
mod fuel_node;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anyhow::Result;
    use eth::{Chain, WebsocketClient};
    use ports::fuel::Api;
    use validator::{BlockValidator, Validator};

    use crate::{committer::Committer, eth_node::EthNode, fuel_node::FuelNode};

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

        let fuel_consensus_pub_key = fuel_node.consensus_pub_key();

        let committer = Committer::default()
            .with_show_logs(false)
            .with_eth_rpc(eth_node.ws_url().clone())
            .with_fuel_rpc(fuel_node.url().clone())
            .with_db_port(db_port)
            .with_db_name(db_name)
            .with_state_contract_address(chain_state_contract_addr)
            .with_fuel_block_producer_public_key(fuel_consensus_pub_key)
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

        let validated_block =
            BlockValidator::new(fuel_consensus_pub_key).validate(&latest_block)?;

        assert!(chain_state_contract.finalized(validated_block).await?);

        Ok(())
    }
}
