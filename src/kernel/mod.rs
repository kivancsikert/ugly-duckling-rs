mod mdns;
mod network;
mod rtc;
mod wifi;

use anyhow::Result;
use embassy_futures::join::join;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::mqtt::client::EspAsyncMqttClient;
use esp_idf_svc::mqtt::client::EspAsyncMqttConnection;
use esp_idf_svc::mqtt::client::QoS;
use esp_idf_svc::sntp::EspSntp;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::AsyncWifi;
use esp_idf_svc::wifi::EspWifi;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use esp_idf_sys::{esp_pm_config_esp32_t, esp_pm_configure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    mqtt: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttClient>>,
    _conn: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>,
    topic_root: String,
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

        // TODO Use some async mDNS instead to avoid blocking the executor
        let mdns = EspMdns::take()?;
        let (sntp, mqtts) = join(
            rtc::init_rtc(&mdns),
            network::init_mqtt(&mdns, &config.instance),
        )
        .await;
        let (mqtt, conn, topic_root) = mqtts?;

        // TODO Figure out how to avoid this warning
        #[allow(clippy::arc_with_non_send_sync)]
        let mqtt = Arc::new(Mutex::new(mqtt));

        Ok(Self {
            config,
            _wifi: wifi,
            _sntp: sntp?,
            mqtt,
            // TODO Do we need to keep this alive?
            _conn: conn,
            topic_root,
        })
    }

    pub async fn publish_mqtt(&self, topic: &str, payload: Value) -> Result<()> {
        let topic = format!("{}/{}", self.topic_root, topic);
        self.mqtt
            .lock()
            .await
            .publish(
                &topic,
                QoS::AtMostOnce,
                false,
                payload.to_string().as_bytes(),
            )
            .await?;
        Ok(())
    }

    pub async fn subscribe_mqtt(&self, topic: &str) -> Result<()> {
        let topic = format!("{}/{}", self.topic_root, topic);
        self.mqtt
            .lock()
            .await
            .subscribe(&topic, QoS::AtMostOnce)
            .await?;
        Ok(())
    }
}

fn load_device_config() -> Result<DeviceConfig> {
    let config_file = include_str!("../../data/device-config.json");
    let config: DeviceConfig = serde_json::from_str(config_file)?;
    log::info!("Loaded config: {:?}", config);
    Ok(config)
}
