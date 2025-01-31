use aws_config::{default_provider::credentials::DefaultCredentialsChain, Region, SdkConfig};
#[cfg(feature = "test-helpers")]
use aws_sdk_kms::config::Credentials;
use aws_sdk_kms::{config::BehaviorVersion, Client};
use k256::{ecdsa::{Signature, VerifyingKey}, pkcs8::DecodePublicKey};

#[cfg(feature = "test-helpers")]
use crate::KeySource;

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

    pub fn url(&self) -> Option<&str> {
        self.sdk_config.endpoint_url()
    }

    pub fn region(&self) -> Option<&Region> {
        self.sdk_config.region()
    }
}

#[derive(Debug, Clone)]
pub struct AwsKmsClient {
    client: Client,
}

impl AwsKmsClient {
    pub async fn new() -> Self {
        let config = AwsConfig::from_env().await;
        let client = Client::new(&config.sdk_config);

        Self { client }
    }

    pub fn inner(&self) -> &Client {
        &self.client
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

        let client = Client::new(&sdk_config);

        Self { client }
    }

    #[cfg(feature = "test-helpers")]
pub async fn create_key(
    &self,
) -> anyhow::Result<KeySource> {
    use std::ops::Deref;

    // Convert hex private key to DER format
    let private_key = hex::decode("***REMOVED***")?;
    let der = k256::SecretKey::from_slice(&private_key)?
        .to_sec1_der()?;
    let der = der.deref().clone();

    // Create custom key with explicit material
    let response = self.client
        .create_key()
        .key_usage(aws_sdk_kms::types::KeyUsageType::SignVerify)
        .key_spec(aws_sdk_kms::types::KeySpec::EccSecgP256K1)
        .customize()
        .mutate_request(move |req| {
            // LocalStack-specific parameter
            req.headers_mut().insert(
                "X-LocalStack-KMS-KeyMaterial",
                base64::encode(der.clone())
            );
        })
        .send()
        .await?;

    let id = response
        .key_metadata
        .and_then(|metadata| metadata.arn)
        .ok_or_else(|| anyhow::anyhow!("key arn missing"))?;

    Ok(KeySource::Kms(id))
}

    // #[cfg(feature = "test-helpers")]
    // pub async fn create_key(&self) -> anyhow::Result<KeySource> {
    //     let response = self
    //         .client
    //         .create_key()
    //         .key_usage(aws_sdk_kms::types::KeyUsageType::SignVerify)
    //         .key_spec(aws_sdk_kms::types::KeySpec::EccSecgP256K1)
    //         .send()
    //         .await?;

    //     // use arn as id to closer imitate prod behavior
    //     let id = response
    //         .key_metadata
    //         .and_then(|metadata| metadata.arn)
    //         .ok_or_else(|| anyhow::anyhow!("key arn missing from response"))?;

    //     Ok(KeySource::Kms(id))
    // }

    pub async fn sign_prehash(&self, key_id: &str, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let response = self
            .client
            .sign()
            .key_id(key_id)
            .message(data.into())
            // use MessageType::Digest because we assume that the data is already hashed
            .message_type(aws_sdk_kms::types::MessageType::Digest)
            .signing_algorithm(aws_sdk_kms::types::SigningAlgorithmSpec::EcdsaSha256)
            .send()
            .await?;

        let der_signature = response
            .signature
            .ok_or_else(|| anyhow::anyhow!("kms signature missing"))?;

        let signature = Signature::from_der(der_signature.as_ref())?;
        let normalized = signature.normalize_s().unwrap_or(signature);
        
        let mut signature_bytes = normalized.to_bytes().to_vec();
        signature_bytes.push(0);

        Ok(signature_bytes)
    }

    pub async fn get_public_key(&self, key_id: &str) -> anyhow::Result<Vec<u8>> {
        let key_info = self.client.get_public_key().key_id(key_id).send().await?;

        let der_bytes: Vec<u8> = key_info
            .public_key
            .ok_or_else(|| anyhow::anyhow!("kms public key missing"))?
            .into();

        // convert to uncompressed form
        let verifying_key = VerifyingKey::from_public_key_der(&der_bytes)?;
        let encoded_point = verifying_key.to_encoded_point(false);

        Ok(encoded_point.as_bytes().to_vec())
    }
}
