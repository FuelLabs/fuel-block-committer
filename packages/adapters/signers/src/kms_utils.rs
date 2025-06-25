use aws_config::{SdkConfig, default_provider::credentials::DefaultCredentialsChain};
use aws_sdk_kms::config::BehaviorVersion;

pub async fn load_config_from_env() -> SdkConfig {
    let loader = aws_config::defaults(BehaviorVersion::latest())
        .credentials_provider(DefaultCredentialsChain::builder().build().await);

    let loader = match std::env::var("E2E_TEST_AWS_ENDPOINT") {
        Ok(url) => loader.endpoint_url(url),
        _ => loader,
    };

    loader.load().await
}

#[cfg(feature = "test-helpers")]
pub async fn config_for_testing(url: String) -> SdkConfig {
    aws_config::defaults(BehaviorVersion::latest())
        .credentials_provider(aws_sdk_kms::config::Credentials::new(
            "test",
            "test",
            None,
            None,
            "Static Credentials",
        ))
        .endpoint_url(url)
        .region(aws_config::Region::new("us-east-1")) // placeholder region for test
        .load()
        .await
}
