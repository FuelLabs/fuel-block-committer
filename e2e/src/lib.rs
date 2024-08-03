#[cfg(test)]
mod committer;
#[cfg(test)]
mod eth_node;
#[cfg(test)]
mod fuel_node;
#[cfg(test)]
mod kms;
#[cfg(test)]
mod whole_stack;

#[cfg(test)]
mod tests {

    use anyhow::Result;

    use ports::fuel::Api;
    use tokio::time::sleep_until;
    use validator::{BlockValidator, Validator};

    use crate::whole_stack::WholeStack;

    #[tokio::test(flavor = "multi_thread")]
    async fn submitted_correct_block_and_was_finalized() -> Result<()> {
        // given
        let show_logs = false;
        // blob support disabled because this test doesn't generate blocks with transactions in it
        // so there is no data to blobify
        let blob_support = false;
        let stack = WholeStack::deploy_default(show_logs, blob_support).await?;

        // when
        stack
            .fuel_node
            .client()
            .produce_blocks(stack.contract_args.blocks_per_interval)
            .await?;

        // then
        stack
            .committer
            .wait_for_committed_block(stack.contract_args.blocks_per_interval as u64)
            .await?;
        let committed_at = tokio::time::Instant::now();

        sleep_until(committed_at + stack.contract_args.finalize_duration).await;

        let latest_block = stack.fuel_node.client().latest_block().await?;

        let validated_block =
            BlockValidator::new(stack.fuel_node.consensus_pub_key()).validate(&latest_block)?;

        assert!(stack.deployed_contract.finalized(validated_block).await?);

        Ok(())
    }
}
