use fuel::HttpClient;
use ports::fuel::FuelPublicKey;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use url::Url;

#[derive(Default, Debug)]
pub struct FuelNode {
    show_logs: bool,
}

pub struct FuelNodeProcess {
    _db_dir: tempfile::TempDir,
    _child: tokio::process::Child,
    url: Url,
    public_key: PublicKey,
}

impl FuelNode {
    pub async fn start(&self) -> anyhow::Result<FuelNodeProcess> {
        let db_dir = tempfile::tempdir()?;
        let unused_port = portpicker::pick_unused_port()
            .ok_or_else(|| anyhow::anyhow!("No free port to start fuel-core"))?;

        let mut cmd = tokio::process::Command::new("fuel-core");

        // let consensus_key =
        let secp = Secp256k1::new();
        let secret_key = SecretKey::new(&mut rand::thread_rng());
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);

        cmd.arg("run")
            .arg("--port")
            .arg(unused_port.to_string())
            .arg("--db-path")
            .arg(db_dir.path())
            .arg("--debug")
            .env(
                "CONSENSUS_KEY_SECRET",
                format!("{}", secret_key.display_secret()),
            )
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());

        let sink = if self.show_logs {
            std::process::Stdio::inherit
        } else {
            std::process::Stdio::null
        };
        cmd.stdout(sink()).stderr(sink());

        let child = cmd.spawn()?;

        let url = format!("http://localhost:{}", unused_port).parse()?;

        let process = FuelNodeProcess {
            _child: child,
            _db_dir: db_dir,
            url,
            public_key,
        };

        process.wait_until_healthy().await;

        Ok(process)
    }

    pub fn with_show_logs(mut self, show_logs: bool) -> Self {
        self.show_logs = show_logs;
        self
    }
}

impl FuelNodeProcess {
    pub fn client(&self) -> HttpClient {
        HttpClient::new(&self.url, 5)
    }

    async fn wait_until_healthy(&self) {
        loop {
            if let Ok(true) = self.client().health().await {
                break;
            }
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn consensus_pub_key(&self) -> FuelPublicKey {
        // We get `FuelPublicKey` from `fuel-core-client` which reexports it from `fuel-core-types`.
        // The below would normally be just a call to `.into()` had `fuel-core-client` enabled/forwarded the `std` flag on its `fuel-core-types` dependency.
        let key_bytes = self.public_key.serialize_uncompressed();
        let mut raw = [0; 64];
        raw.copy_from_slice(&key_bytes[1..]);
        serde_json::from_str(&format!("\"{}\"", hex::encode(raw))).expect("valid fuel pub key")
    }
}
