use super::*;
/// Shared application data for the control panel.
pub struct AppData {
    pub simulation_config: Arc<Mutex<SimulationConfig>>,
    pub metrics_url: String,
}

/// Form definition for updating the configuration.
#[derive(Debug, Deserialize)]
pub struct ConfigForm {
    pub block_size: usize,
    pub compressibility: String,
}
