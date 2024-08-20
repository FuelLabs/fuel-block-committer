use alloy::signers::{aws::AwsSigner, Signer};
use anyhow::Context;
use eth::{Address, AwsClient};
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

        let client = AwsClient::new().await;

        Ok(KmsProcess {
            _container: container,
            client,
        })
    }
}

fn spawn_log_printer(container: &testcontainers::ContainerAsync<KmsImage>) {
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

pub struct KmsProcess {
    _container: testcontainers::ContainerAsync<KmsImage>,
    client: AwsClient,
}

#[derive(Debug, Clone)]
pub struct KmsKey {
    pub id: String,
    pub signer: AwsSigner,
}

impl KmsKey {
    pub fn address(&self) -> Address {
        self.signer.address()
    }
}

impl KmsProcess {
    pub async fn create_key(&self, chain: u64) -> anyhow::Result<KmsKey> {
        let response = self
            .client
            .inner()
            .create_key()
            .key_usage(aws_sdk_kms::types::KeyUsageType::SignVerify)
            .key_spec(aws_sdk_kms::types::KeySpec::EccSecgP256K1)
            .send()
            .await?;

        let id = response
            .key_metadata
            .ok_or_else(|| anyhow::anyhow!("key id missing from response"))?
            .key_id;

        let signer = self.client.make_signer(id.clone(), chain).await?;

        Ok(KmsKey { id, signer })
    }
}
