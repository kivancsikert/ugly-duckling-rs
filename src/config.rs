use anyhow::Result;
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, Debug)]
pub struct DeviceConfig {
    pub instance: String,
    pub id: String,
}

pub fn load_device_config() -> Result<DeviceConfig> {
    let config_file = include_str!("../data/device-config.json");
    let config: DeviceConfig = serde_json::from_str(config_file)?;
    log::info!("Loaded config: {:?}", config);
    Ok(config)
}
