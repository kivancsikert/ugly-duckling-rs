pub mod command;
mod mdns;
mod mqtt;
mod rtc;
mod wifi;

use anyhow::anyhow;
use anyhow::Result;
use embassy_futures::join::join;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::sntp::EspSntp;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::AsyncWifi;
use esp_idf_svc::wifi::EspWifi;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use esp_idf_sys::{esp_pm_config_esp32_t, esp_pm_configure};
use mqtt::IncomingMessageResponse;
use mqtt::Mqtt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;
use static_cell::StaticCell;
use std::ffi::c_void;
use std::sync::Arc;

// TODO Configure these per device model
const MAX_FREQ_MHZ: i32 = 160;
const MIN_FREQ_MHZ: i32 = 40;

#[derive(Serialize, Deserialize, Debug)]
pub struct DeviceConfig {
    pub instance: String,
    pub id: String,
    #[serde(rename = "sleepWhenIdle", default)]
    sleep_when_idle: bool,
}

pub struct Device {
    pub config: DeviceConfig,
    _wifi: Arc<Mutex<CriticalSectionRawMutex, AsyncWifi<EspWifi<'static>>>>,
    _sntp: EspSntp<'static>,
    pub mqtt: Mqtt,
}

impl Device {
    pub async fn init(modem: Modem) -> Result<Self> {
        let sys_loop = EspSystemEventLoop::take()?;
        let timer_service = EspTaskTimerService::new()?;
        let nvs = EspDefaultNvsPartition::take()?;

        let config = load_device_config()?;

        if config.sleep_when_idle {
            log::info!("Device will sleep when idle");
        } else {
            log::info!("Device will not sleep when idle");
        }
        let pm_config = esp_pm_config_esp32_t {
            max_freq_mhz: MAX_FREQ_MHZ,
            min_freq_mhz: MIN_FREQ_MHZ,
            light_sleep_enable: config.sleep_when_idle,
        };
        esp_idf_sys::esp!(unsafe { esp_pm_configure(&pm_config as *const _ as *mut c_void) })?;

        let wifi =
            wifi::init_wifi(&config.instance, modem, &sys_loop, &timer_service, &nvs).await?;

        // TODO Use something better than a static cell
        static COMMAND_MANAGER: StaticCell<command::CommandManager> = StaticCell::new();
        let command_manager = COMMAND_MANAGER.init(command::CommandManager::new());
        command_manager.register("ping", |v: Value| {
            log::info!("Ping received: {:?}", v);
            Ok(Some(json!({"pong": v})))
        });

        // TODO Use some async mDNS instead to avoid blocking the executor
        let mdns = EspMdns::take()?;
        let (sntp, mqtt) = join(
            rtc::init_rtc(&mdns),
            mqtt::Mqtt::create(
                &mdns,
                &config.instance,
                Box::new(|path, payload| -> Result<IncomingMessageResponse> {
                    if let Some(command) = path.strip_prefix("commands/") {
                        let result = command_manager.handle(command, payload);
                        match result {
                            Ok(Some(response)) => {
                                log::info!("Command response: {}", response);
                                Ok(Some((format!("responses/{command}"), response)))
                            }
                            Ok(None) => {
                                log::info!("Command response: None");
                                Ok(None)
                            }
                            Err(e) => Err(anyhow!("Command error: {}", e)),
                        }
                    } else {
                        log::info!("Not a command: {}", path);
                        Ok(None)
                    }
                }),
            ),
        )
        .await;
        let mqtt = mqtt?;

        mqtt.subscribe("commands/#").await?;

        Ok(Self {
            config,
            _wifi: wifi,
            _sntp: sntp?,
            mqtt,
        })
    }
}

fn load_device_config() -> Result<DeviceConfig> {
    let config_file = include_str!("../../data/device-config.json");
    let config: DeviceConfig = serde_json::from_str(config_file)?;
    log::info!("Loaded config: {:?}", config);
    Ok(config)
}
