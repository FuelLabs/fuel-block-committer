use anyhow::Context;
use eth::{AwsClient, AwsCredentialsProvider, AwsRegion};
use ethers::signers::{AwsSigner, Signer};
use ports::types::H160;
use rusoto_core::Region;
use rusoto_kms::{CreateKeyRequest, Kms as RusotoKms};
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
            spawn_log_printer(&container);
        }

        let port = container.get_host_port_ipv4(4566).await?;

        let region = AwsRegion::from(Region::Custom {
            name: "us-east-2".to_string(),
            endpoint: format!("http://localhost:{}", port),
        });
        let credentials = AwsCredentialsProvider::new_static("test", "test");
        let client = AwsClient::try_new(true, region.clone(), credentials)?;

        Ok(KmsProcess {
            _container: container,
            client,
            region,
        })
    }
}

fn spawn_log_printer(container: &testcontainers::ContainerAsync<KmsImage>) {
    let stderr = container.stderr(true);
    let stdout = container.stdout(true);
    tokio::spawn(async move {
        let mut stderr_lines = stderr.lines();
        let mut stdout_lines = stdout.lines();

        let mut other_stream_closed = false;
        loop {
            tokio::select! {
                stderr_result = stderr_lines.next_line() => {
                    match stderr_result {
                        Ok(Some(line)) => eprintln!("KMS (stderr): {}", line),
                        Ok(None) if other_stream_closed => break,
                        Ok(None) => other_stream_closed=true,
                        Err(e) => {
                            eprintln!("KMS: Error reading from stderr: {:?}", e);
                            break;
                        }
                    }
                }
                stdout_result = stdout_lines.next_line() => {
                    match stdout_result {
                        Ok(Some(line)) => eprintln!("KMS (stdout): {}", line),
                        Ok(None) if other_stream_closed => break,
                        Ok(None) => other_stream_closed=true,
                        Err(e) => {
                            eprintln!("KMS: Error reading from stdout: {:?}", e);
                            break;
                        }
                    }
                }
            }
        }

        Ok::<(), std::io::Error>(())
    });
}

pub struct KmsProcess {
    _container: testcontainers::ContainerAsync<KmsImage>,
    client: AwsClient,
    region: AwsRegion,
}

#[derive(Debug, Clone)]
pub struct KmsKey {
    pub id: String,
    pub signer: AwsSigner,
    pub region: AwsRegion,
}

impl KmsKey {
    pub fn address(&self) -> H160 {
        self.signer.address()
    }
}

impl KmsProcess {
    pub async fn create_key(&self, chain: u64) -> anyhow::Result<KmsKey> {
        let response = self
            .client
            .inner()
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

        let signer = self.client.make_signer(id.clone(), chain).await?;

        Ok(KmsKey {
            id,
            signer,
            region: self.region.clone(),
        })
    }

    pub fn region(&self) -> &AwsRegion {
        &self.region
    }
}
