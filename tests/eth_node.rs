use std::{
    io::{BufRead, BufReader},
    process::Stdio,
    sync::Arc,
};

use os_pipe::PipeReader;
use tokio::{
    process::{Child, Command},
    sync::Notify,
    task::JoinHandle,
};

pub struct EthNode {
    _process: Child,
    _process_output_processor: JoinHandle<()>,
    contracts_deployed_notif: Arc<Notify>,
    config: EthNodeConfig,
}

pub struct EthNodeConfig {
    pub port: u16,
    pub print_output: bool,
}

impl Default for EthNodeConfig {
    fn default() -> Self {
        Self {
            port: 8545,
            print_output: false,
        }
    }
}

impl EthNode {
    pub fn run(config: EthNodeConfig) -> Self {
        let port = config.port;

        let (reader, stdout) = os_pipe::pipe().unwrap();
        let stderr = stdout.try_clone().unwrap();

        let child = Command::new("docker")
            .args(["run", "-p", &format!("{port}:8545"), "eth_node"])
            .kill_on_drop(true)
            .stdout(stdout)
            .stderr(stderr)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();

        let (process_output_processor, notify_ready) =
            Self::setup_watch_on_output(reader, config.print_output);

        Self {
            _process: child,
            config,
            _process_output_processor: process_output_processor,
            contracts_deployed_notif: notify_ready,
        }
    }

    pub async fn wait_until_ready(&self) {
        self.contracts_deployed_notif.notified().await;
    }

    fn setup_watch_on_output(
        child: PipeReader,
        print_output: bool,
    ) -> (JoinHandle<()>, Arc<Notify>) {
        let rx = Arc::new(Notify::new());
        let tx = Arc::clone(&rx);

        let handle = tokio::task::spawn_blocking(move || {
            let mut reader = BufReader::new(child).lines().into_iter();
            let mut notified = false;
            while let Some(Ok(line)) = reader.next() {
                if print_output {
                    eprintln!("{line}");
                }

                if !notified
                    && line.contains(
                        "FuelERC20Gateway_impl: 0x58Bf5211B9A7176aeAFa7129516c4D7C83696951",
                    )
                {
                    tx.notify_one();
                    notified = true;

                    if !print_output {
                        break;
                    }
                }
            }
        });

        (handle, rx)
    }

    pub fn port(&self) -> u16 {
        self.config.port
    }
}
