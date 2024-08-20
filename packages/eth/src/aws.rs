use alloy::signers::aws::AwsSigner;
use aws_sdk_kms::config::BehaviorVersion;

#[derive(Clone)]
pub struct AwsClient {
    client: aws_sdk_kms::Client,
}

impl AwsClient {
    pub async fn new() -> Self {
        let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
        let client = aws_sdk_kms::Client::new(&config);

        Self { client }
    }

    pub fn inner(&self) -> &aws_sdk_kms::Client {
        &self.client
    }

    pub async fn make_signer(&self, key_id: String, chain_id: u64) -> ports::l1::Result<AwsSigner> {
        AwsSigner::new(self.client.clone(), key_id, Some(chain_id))
            .await
            .map_err(|err| ports::l1::Error::Other(format!("Error making aws signer: {err}")))
    }
}
