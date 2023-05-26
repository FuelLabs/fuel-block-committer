#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _config = fuel_block_committer_cli::parse()?;
    Ok(())
}
