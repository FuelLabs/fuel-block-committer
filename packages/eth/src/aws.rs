use alloy::signers::aws::AwsSigner;
use aws_config::{default_provider::credentials::DefaultCredentialsChain, Region, SdkConfig};
use aws_sdk_kms::{
    config::{BehaviorVersion, Credentials},
    Client,
};

use crate::error::Error;

#[derive(Debug, Clone)]
pub enum AwsConfig {
    Prod(Region),
    Test(String),
}

impl AwsConfig {
    pub fn from_env() -> crate::error::Result<Self> {
        read_aws_test_url()
            .or_else(read_aws_prod_region)
            .ok_or_else(|| Error::Other("No AWS region found".to_string()))
    }

    pub fn url(&self) -> Option<String> {
        match self {
            AwsConfig::Prod(_) => None,
            AwsConfig::Test(url) => Some(url.clone()),
        }
    }

    pub fn as_region(&self) -> Region {
        match self {
            AwsConfig::Prod(region) => region.clone(),
            AwsConfig::Test(_) => Region::new("us-east-1"),
        }
    }

    pub async fn load(&self) -> SdkConfig {
        let loader = aws_config::defaults(BehaviorVersion::latest()).region(self.as_region());

        let loader = match self {
            AwsConfig::Prod(_) => {
                loader.credentials_provider(DefaultCredentialsChain::builder().build().await)
            }
            AwsConfig::Test(url) => {
                let credentials =
                    Credentials::new("test", "test", None, None, "Static Credentials");
                loader.credentials_provider(credentials).endpoint_url(url)
            }
        };

        loader.load().await
    }
}

fn read_aws_test_url() -> Option<AwsConfig> {
    let env_value = std::env::var("E2E_TEST_AWS_ENDPOINT").ok()?;
    Some(AwsConfig::Test(env_value))
}

fn read_aws_prod_region() -> Option<AwsConfig> {
    let env_value = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .ok()?;
    Some(AwsConfig::Prod(Region::new(env_value)))
}

#[derive(Clone)]
pub struct AwsClient {
    client: Client,
}

impl AwsClient {
    pub async fn new(config: AwsConfig) -> Self {
        let config = config.load().await;
        let client = Client::new(&config);

        Self { client }
    }

    pub fn inner(&self) -> &Client {
        &self.client
    }

    pub async fn make_signer(&self, key_id: String, chain_id: u64) -> ports::l1::Result<AwsSigner> {
        AwsSigner::new(self.client.clone(), key_id, Some(chain_id))
            .await
            .map_err(|err| ports::l1::Error::Other(format!("Error making aws signer: {err}")))
    }
}
