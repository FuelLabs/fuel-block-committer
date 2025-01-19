use alloy::signers::aws::AwsSigner;
use aws_config::{default_provider::credentials::DefaultCredentialsChain, Region, SdkConfig};
#[cfg(feature = "test-helpers")]
use aws_sdk_kms::config::Credentials;
use aws_sdk_kms::{config::BehaviorVersion, Client};
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
        // ... existing implementation ...
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
    // ... existing implementation ...
}