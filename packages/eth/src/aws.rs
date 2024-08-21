use alloy::signers::aws::AwsSigner;
use aws_config::{environment::EnvironmentVariableCredentialsProvider, meta::credentials::CredentialsProviderChain, Region};
use aws_sdk_kms::config::{BehaviorVersion, Credentials};

use crate::error::Error;

#[derive(Debug, Clone)]
pub enum AwsRegion {
    Prod(Region),
    Test(String),
}

impl AwsRegion {
    pub fn from_env() -> crate::error::Result<Self> {
        read_aws_test_region()
            .or_else(read_aws_prod_region)
            .ok_or_else(|| Error::Other("No AWS region found".to_string()))
    }

    pub fn url(&self) -> Option<String> {
        match self {
            AwsRegion::Prod(_) => None,
            AwsRegion::Test(region) => Some(region.clone()),
        }
    }

    pub fn as_region(&self) -> Region {
        match self {
            AwsRegion::Prod(region) => region.clone(),
            AwsRegion::Test(_) => Region::new("us-east-1"),
        }
    }
}

fn read_aws_test_region() -> Option<AwsRegion> {
    let env_value = std::env::var("E2E_TEST_AWS_REGION").ok()?;
    Some(AwsRegion::Test(env_value))
}

fn read_aws_prod_region() -> Option<AwsRegion> {
    let env_value = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .ok()?;
    Some(AwsRegion::Prod(Region::new(env_value)))
}

#[derive(Clone)]
pub struct AwsClient {
    client: aws_sdk_kms::Client,
}

impl AwsClient {
    pub async fn new(region: AwsRegion) -> Self {
        let credentials = Credentials::new("test", "test", None, None, "Static Credentials");
        let loader = aws_config::defaults(BehaviorVersion::latest())
            .region(region.as_region())
            .credentials_provider(credentials);

        let loader = if let Some(url) = region.url() {
            loader.endpoint_url(url)
        } else {
            loader
        };

        let config = loader.load().await;
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
