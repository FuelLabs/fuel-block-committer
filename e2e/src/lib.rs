#[cfg(test)]
mod tests {
    use anyhow::{anyhow, Result};
    use eth::{Chain, WebsocketClient};
    use fuel::HttpClient;
    use ports::fuel::{Api, FuelPublicKey};
    use ports::l1::Contract;
    use std::time::Duration;
    use validator::{BlockValidator, Validator};

    const FUEL_NODE_PORT: u16 = 4000;

    #[tokio::test(flavor = "multi_thread")]
    async fn submitted_correct_block_and_was_finalized() -> Result<()> {
        let fuel_node_address = format!("http://localhost:{FUEL_NODE_PORT}");
        let provider = HttpClient::new(&fuel_node_address.parse()?, 10);

        let fuel_contract = WebsocketClient::connect(
            &"ws://localhost:8089".parse()?,
            Chain::AnvilHardhat,
            "0xdAad669b06d79Cb48C8cfef789972436dBe6F24d".parse()?,
            "0x9e56ccf010fa4073274b8177ccaad46fbaf286645310d03ac9bb6afa922a7c36",
            10,
        )
        .await?;

        provider
            .produce_blocks(fuel_contract.commit_interval().into())
            .await?;

        // time enough to fwd the block to ethereum and for the TIME_TO_FINALIZE (1s) to elapse
        tokio::time::sleep(Duration::from_secs(5)).await;

        let latest_block = provider.latest_block().await?;

        // Had to use `serde_json` because `FromStr` did not work because of an validation error
        let producer_public_key: FuelPublicKey = serde_json::from_str("\"0x73dc6cc8cc0041e4924954b35a71a22ccb520664c522198a6d31dc6c945347bb854a39382d296ec64c70d7cea1db75601595e29729f3fbdc7ee9dae66705beb4\"")
                 .map_err(|e| anyhow!("could not parse producer pub key: {e:?}"))?;

        let validated_block = BlockValidator::new(producer_public_key).validate(&latest_block)?;

        assert!(fuel_contract.finalized(validated_block).await?);

        Ok(())
    }
}
