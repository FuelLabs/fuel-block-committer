use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use rand::{rngs::SmallRng, thread_rng, Rng, RngCore, SeedableRng};
use tokio::{
    sync::Mutex,
    time::{sleep, Duration},
};

use super::graphql::{block_at_height, Block, DaCompressedBlock, HexString};

pub struct AppState {
    pub current_height: AtomicU32,
    pub block_contents: Mutex<Vec<u8>>,
}

impl AppState {
    pub fn new(block_size: usize) -> Self {
        let mut rng = SmallRng::from_seed([0; 32]);
        let mut block = vec![0; block_size];
        rng.fill_bytes(&mut block);
        Self {
            current_height: AtomicU32::new(0),
            block_contents: Mutex::new(block),
        }
    }

    pub fn latest_block(&self) -> Block {
        block_at_height(self.current_height.load(Ordering::Relaxed))
    }

    pub async fn compressed_block(&self, _height: u32) -> DaCompressedBlock {
        let block_contents = self.block_contents.lock().await;
        let compressed = format!("0x{}", hex::encode(&*block_contents));
        DaCompressedBlock {
            bytes: HexString(compressed),
        }
    }
}

#[derive(Clone)]
pub struct SimulationConfig {
    pub block_size: usize,
    pub compressibility: Compressibility,
}

impl SimulationConfig {
    pub fn new(block_size: usize, compressibility: Compressibility) -> Self {
        Self {
            block_size,
            compressibility,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Compressibility {
    Random,
    Low,
    Medium,
    High,
    Full,
}

impl std::str::FromStr for Compressibility {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "random" => Ok(Compressibility::Random),
            "low" => Ok(Compressibility::Low),
            "medium" => Ok(Compressibility::Medium),
            "high" => Ok(Compressibility::High),
            "full" => Ok(Compressibility::Full),
            _ => Err(format!("Invalid compressibility option: {}", s)),
        }
    }
}

impl std::fmt::Display for Compressibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Compressibility::Random => "Random",
            Compressibility::Low => "Low",
            Compressibility::Medium => "Medium",
            Compressibility::High => "High",
            Compressibility::Full => "Full",
        };
        write!(f, "{}", label)
    }
}

pub fn generate_block_contents(block_size: usize, compressibility: &Compressibility) -> Vec<u8> {
    let mut rng = thread_rng();
    let mut block = vec![0u8; block_size];
    match compressibility {
        Compressibility::Random => {
            rng.fill_bytes(&mut block);
        }
        Compressibility::Full => {
            // All bytes remain zero.
        }
        Compressibility::Low => {
            rng.fill_bytes(&mut block);
            for byte in block.iter_mut() {
                if rng.gen_bool(0.25) {
                    *byte = 0;
                }
            }
        }
        Compressibility::Medium => {
            rng.fill_bytes(&mut block);
            for byte in block.iter_mut() {
                if rng.gen_bool(0.50) {
                    *byte = 0;
                }
            }
        }
        Compressibility::High => {
            rng.fill_bytes(&mut block);
            for byte in block.iter_mut() {
                if rng.gen_bool(0.75) {
                    *byte = 0;
                }
            }
        }
    }
    block
}

pub async fn produce_blocks(state: Arc<AppState>, config: Arc<Mutex<SimulationConfig>>) {
    loop {
        sleep(Duration::from_secs(1)).await;
        // Get the current simulation configuration.
        let current_config = {
            let cfg = config.lock().await;
            (cfg.block_size, cfg.compressibility.clone())
        };
        // Generate new block contents.
        let new_block = generate_block_contents(current_config.0, &current_config.1);
        {
            let mut block_contents = state.block_contents.lock().await;
            *block_contents = new_block;
        }
        state.current_height.fetch_add(1, Ordering::Relaxed);
    }
}
