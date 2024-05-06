#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anyhow::Result;
    use eth::{Chain, WebsocketClient};
    use fuel::HttpClient;
    use ports::fuel::Api;

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
            3.try_into()?,
            10,
        )
        .await?;

        provider.produce_blocks(3).await?;

        // time enough to fwd the block to ethereum and for the TIME_TO_FINALIZE (1s) to elapse
        tokio::time::sleep(Duration::from_secs(5)).await;

        let latest_block = provider.latest_block().await?;

        assert!(fuel_contract.finalized(latest_block).await?);

        Ok(())
    }
}
