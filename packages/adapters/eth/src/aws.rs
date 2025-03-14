use alloy::signers::aws::AwsSigner;
use aws_config::{Region, SdkConfig, default_provider::credentials::DefaultCredentialsChain};
#[cfg(feature = "test-helpers")]
use aws_sdk_kms::config::Credentials;
use aws_sdk_kms::{Client, config::BehaviorVersion};
use services::{Error, Result};

#[derive(Debug, Clone)]
pub struct AwsConfig {
    sdk_config: SdkConfig,
}

impl AwsConfig {
    pub async fn from_env() -> Self {
        let loader = aws_config::defaults(BehaviorVersion::latest())
            .credentials_provider(DefaultCredentialsChain::builder().build().await);

        let loader = match std::env::var("E2E_TEST_AWS_ENDPOINT") {
            Ok(url) => loader.endpoint_url(url),
            _ => loader,
        };

        Self {
            sdk_config: loader.load().await,
        }
    }

    #[cfg(feature = "test-helpers")]
    pub async fn for_testing(url: String) -> Self {
        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .credentials_provider(Credentials::new(
                "test",
                "test",
                None,
                None,
                "Static Credentials",
            ))
            .endpoint_url(url)
            .region(Region::new("us-east-1")) // placeholder region for test
            .load()
            .await;

        Self { sdk_config }
    }

    pub fn url(&self) -> Option<&str> {
        self.sdk_config.endpoint_url()
    }

    pub fn region(&self) -> Option<&Region> {
        self.sdk_config.region()
    }
}

#[derive(Clone)]
pub struct AwsClient {
    client: Client,
}

impl AwsClient {
    pub fn new(config: AwsConfig) -> Self {
        let config = config.sdk_config;
        let client = Client::new(&config);

        Self { client }
    }

    pub fn inner(&self) -> &Client {
        &self.client
    }

    pub async fn make_signer(&self, key_arn: String) -> Result<AwsSigner> {
        AwsSigner::new(self.client.clone(), key_arn, None)
            .await
            .map_err(|err| Error::Other(format!("Error making aws signer: {err:?}")))
    }
}
