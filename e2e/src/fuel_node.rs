use fuel::HttpClient;
use url::Url;

pub struct FuelNode {}

pub struct FuelNodeProcess {
    _db_dir: tempfile::TempDir,
    _child: tokio::process::Child,
    url: Url,
}

impl FuelNode {
    pub async fn start() -> anyhow::Result<FuelNodeProcess> {
        let db_dir = tempfile::tempdir()?;
        let unused_port = portpicker::pick_unused_port()
            .ok_or_else(|| anyhow::anyhow!("No free port to start fuel-core"))?;

        let child = tokio::process::Command::new("fuel-core")
            .arg("run")
            .arg("--port")
            .arg(unused_port.to_string())
            .arg("--db-path")
            .arg(db_dir.path())
            .arg("--debug")
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        let url = format!("http://localhost:{}", unused_port).parse()?;

        let process = FuelNodeProcess {
            _child: child,
            _db_dir: db_dir,
            url,
        };

        process.wait_until_healthy().await;

        Ok(process)
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
}
