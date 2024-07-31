use anyhow::Context;
use eth::Chain;
use ethers::signers::{AwsSigner, Signer};
use ethers::{abi::Address, middleware::Middleware};
use ports::types::H160;
use rusoto_core::{HttpClient, Region};
use rusoto_kms::{CreateKeyRequest, Kms as RusotoKms, KmsClient};
use testcontainers::{core::ContainerPort, runners::AsyncRunner};
use tokio::io::AsyncBufReadExt;

#[derive(Default)]
pub struct Kms {
    show_logs: bool,
}

struct KmsImage;

impl testcontainers::Image for KmsImage {
    fn name(&self) -> &str {
        "localstack/localstack"
    }

    fn tag(&self) -> &str {
        "latest"
    }

    fn ready_conditions(&self) -> Vec<testcontainers::core::WaitFor> {
        vec![testcontainers::core::WaitFor::message_on_stdout("Ready.")]
    }

    fn expose_ports(&self) -> &[ContainerPort] {
        &const { [ContainerPort::Tcp(4566)] }
    }
}

impl Kms {
    pub fn with_show_logs(mut self, show_logs: bool) -> Self {
        self.show_logs = show_logs;
        self
    }

    pub async fn start(self) -> anyhow::Result<KmsProcess> {
        let container = KmsImage
            .start()
            .await
            .with_context(|| "Failed to start KMS container")?;

        if self.show_logs {
            let stderr = container.stderr(true);
            let stdout = container.stdout(true);
            tokio::spawn(async move {
                let mut stderr_lines = stderr.lines();
                let mut stdout_lines = stdout.lines();

                tokio::select! {
                    _ = async {
                        while let Ok(Some(line)) = stderr_lines.next_line().await {
                            eprintln!("KMS: {}", line);
                        }
                    } => {}
                    _ = async {
                        while let Ok(Some(line)) = stdout_lines.next_line().await {
                            eprintln!("KMS: {}", line);
                        }
                    } => {}
                }

                Ok::<_, anyhow::Error>(())
            });
        }
        let port = container.get_host_port_ipv4(4566).await?;

        let region = Region::Custom {
            name: "us-east-2".to_string(),
            endpoint: format!("http://localhost:{}", port),
        };
        let credentials = rusoto_core::credential::StaticProvider::new_minimal(
            "test".to_string(),
            "test".to_string(),
        );
        let hyper_builder = hyper::client::Client::builder();
        let http_connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .build();
        let dispatcher = HttpClient::from_builder(hyper_builder, http_connector);
        let client = KmsClient::new_with(dispatcher, credentials, region.clone());

        Ok(KmsProcess {
            container,
            client,
            region,
        })
    }
}

pub struct KmsProcess {
    container: testcontainers::ContainerAsync<KmsImage>,
    client: KmsClient,
    region: Region,
}

#[derive(Debug, Clone)]
pub struct KmsKey {
    pub id: String,
    pub signer: AwsSigner,
    pub region: Region,
}

impl KmsKey {
    pub fn address(&self) -> H160 {
        self.signer.address()
    }
}

impl KmsProcess {
    pub async fn create_key(&self, chain: u64) -> anyhow::Result<KmsKey> {
        eprintln!("sending the request");
        let response = self
            .client
            .create_key(CreateKeyRequest {
                customer_master_key_spec: Some("ECC_SECG_P256K1".to_string()),
                key_usage: Some("SIGN_VERIFY".to_string()),
                ..Default::default()
            })
            .await?;

        let id = response
            .key_metadata
            .ok_or_else(|| anyhow::anyhow!("key id missing from response"))?
            .key_id;

        eprintln!("creating the signer");
        let signer =
            ethers::signers::AwsSigner::new(self.client.clone(), id.clone(), chain).await?;

        Ok(KmsKey {
            id,
            signer,
            region: self.region.clone(),
        })
    }

    pub fn region(&self) -> &Region {
        &self.region
    }
}
