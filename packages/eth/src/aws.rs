use ethers::signers::AwsSigner;
#[cfg(feature = "test-helpers")]
use rusoto_core::credential::StaticProvider;
use rusoto_core::Region;
use rusoto_core::{
    credential::{
        AwsCredentials, ContainerProvider, CredentialsError, EnvironmentProvider,
        InstanceMetadataProvider, ProfileProvider, ProvideAwsCredentials,
    },
    HttpClient,
};
use rusoto_kms::KmsClient;
use rusoto_sts::WebIdentityProvider;

#[derive(Debug, Clone)]
pub struct AwsRegion {
    region: Region,
}

impl serde::Serialize for AwsRegion {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.region.serialize(serializer)
    }
}

impl From<Region> for AwsRegion {
    fn from(value: Region) -> Self {
        Self { region: value }
    }
}

impl From<AwsRegion> for Region {
    fn from(value: AwsRegion) -> Self {
        value.region
    }
}

impl Default for AwsRegion {
    fn default() -> Self {
        // We first try to deserialize the env region as json because `Region::default` doesn't
        // handle custom regions needed for e2e tests
        let region = region_given_as_json_in_env().unwrap_or_default();

        Self { region }
    }
}

fn region_given_as_json_in_env() -> Option<Region> {
    let env_value = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .ok()?;
    serde_json::from_str::<Region>(&env_value).ok()
}

#[derive(Clone)]
pub struct AwsClient {
    client: KmsClient,
}

impl AwsClient {
    pub fn try_new(
        allow_http: bool,
        region: AwsRegion,
        credentials: AwsCredentialsProvider,
    ) -> ports::l1::Result<Self> {
        let dispatcher = if allow_http {
            let hyper_builder = hyper::client::Client::builder();
            let http_connector = hyper_rustls::HttpsConnectorBuilder::new()
                .with_native_roots()
                .https_or_http()
                .enable_http1()
                .build();
            HttpClient::from_builder(hyper_builder, http_connector)
        } else {
            HttpClient::new().map_err(|e| {
                ports::l1::Error::Network(format!("Could not create http client: {e}"))
            })?
        };

        let client = KmsClient::new_with(dispatcher, credentials, region.into());
        Ok(Self { client })
    }

    pub fn inner(&self) -> &KmsClient {
        &self.client
    }

    pub async fn make_signer(&self, key_id: String, chain_id: u64) -> ports::l1::Result<AwsSigner> {
        AwsSigner::new(self.client.clone(), key_id, chain_id)
            .await
            .map_err(|err| ports::l1::Error::Other(format!("Error making aws signer: {err}")))
    }
}

#[derive(Debug, Clone)]
pub struct AwsCredentialsProvider {
    credentials: CredentialsProvider,
}

impl AwsCredentialsProvider {
    #[cfg(feature = "test-helpers")]
    pub fn new_static(access_key: impl Into<String>, secret_access_key: impl Into<String>) -> Self {
        Self {
            credentials: CredentialsProvider::Static(StaticProvider::new(
                access_key.into(),
                secret_access_key.into(),
                None,
                None,
            )),
        }
    }

    pub fn new_chain() -> Self {
        Self {
            credentials: CredentialsProvider::Chain {
                environment_provider: EnvironmentProvider::default(),
                profile_provider: ProfileProvider::new().ok(),
                instance_metadata_provider: InstanceMetadataProvider::new(),
                container_provider: ContainerProvider::new(),
                web_identity_provider: WebIdentityProvider::from_k8s_env(),
            },
        }
    }
}

#[derive(Clone, Debug)]
enum CredentialsProvider {
    #[cfg(feature = "test-helpers")]
    Static(StaticProvider),
    Chain {
        environment_provider: EnvironmentProvider,
        instance_metadata_provider: InstanceMetadataProvider,
        container_provider: ContainerProvider,
        profile_provider: Option<ProfileProvider>,
        web_identity_provider: WebIdentityProvider,
    },
}

#[async_trait::async_trait]
impl ProvideAwsCredentials for AwsCredentialsProvider {
    async fn credentials(&self) -> std::result::Result<AwsCredentials, CredentialsError> {
        match &self.credentials {
            #[cfg(feature = "test-helpers")]
            CredentialsProvider::Static(provider) => provider.credentials().await,
            CredentialsProvider::Chain {
                environment_provider,
                instance_metadata_provider,
                container_provider,
                profile_provider,
                web_identity_provider,
            } => {
                // Copied from rusoto_core::credential::ChainProvider
                if let Ok(creds) = environment_provider.credentials().await {
                    return Ok(creds);
                }
                if let Some(ref profile_provider) = profile_provider {
                    if let Ok(creds) = profile_provider.credentials().await {
                        return Ok(creds);
                    }
                }
                if let Ok(creds) = container_provider.credentials().await {
                    return Ok(creds);
                }
                if let Ok(creds) = instance_metadata_provider.credentials().await {
                    return Ok(creds);
                }

                // Added by us
                if let Ok(creds) = web_identity_provider.credentials().await {
                    return Ok(creds);
                }

                Err(CredentialsError::new(
                    "Couldn't find AWS credentials in environment, credentials file, or IAM role.",
                ))
            }
        }
    }
}
