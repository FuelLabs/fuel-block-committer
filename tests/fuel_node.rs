use std::net::SocketAddr;

use fuels::{
    prelude::Provider,
    test_helpers::{setup_test_provider, Config},
};

pub struct FuelNode {
    pub provider: Provider,
    addr: SocketAddr,
}

impl FuelNode {
    pub async fn run() -> Self {
        let node_config = Config {
            manual_blocks_enabled: true,
            ..Config::local_node()
        };

        let (provider, addr) =
            setup_test_provider(vec![], vec![], Some(node_config), Some(Default::default())).await;

        Self { provider, addr }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }
}
