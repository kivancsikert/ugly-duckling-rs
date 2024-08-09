mod network;

use anyhow::Result;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::mqtt::client::{EspAsyncMqttClient, MessageId};
use esp_idf_svc::sntp::{self, EspSntp};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::EspWifi;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use esp_idf_sys::{esp_pm_config_esp32_t, esp_pm_configure};
use network::{query_mdns, Service};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;
use std::ffi::c_void;

pub struct Device {
    pub config: DeviceConfig,
    _wifi: EspWifi<'static>,
    _sntp: EspSntp<'static>,
    topic_root: String,
    mqtt: RefCell<EspAsyncMqttClient>,
}

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
            network::connect_wifi(&config.instance, modem, &sys_loop, &timer_service, &nvs).await?;
        let ip_info = wifi.sta_netif().get_ip_info()?;
        log::info!("WiFi DHCP info: {:?}", ip_info);

        // TODO Use some async mDNS instead to avoid blocking the executor
        let mdns = EspMdns::take()?;

        let ntp = query_mdns(&mdns, "_ntp", "_udp")?.unwrap_or_else(|| Service {
            hostname: String::from("pool.ntp.org"),
            port: 123,
        });
        log::info!(
            "Time before SNTP sync: {:?}, synchronizing with {:?}",
            std::time::SystemTime::now(),
            ntp
        );

        let sntp = sntp::EspSntp::new_with_callback(
            &sntp::SntpConf {
                servers: [ntp.hostname.as_str()],
                ..Default::default()
            },
            |synced_time| {
                log::info!("Time synced via SNTP: {:?}", synced_time);
            },
        )?;

        let mqtt = query_mdns(&mdns, "_mqtt", "_tcp")?.unwrap_or_else(|| Service {
            hostname: String::from("bumblebee.local"),
            port: 1883,
        });
        log::info!("MDNS query result: {:?}", mqtt);

        let (mqtt, _) = network::connect_mqtt(
            &format!("mqtt://{}:{}", mqtt.hostname, mqtt.port),
            &config.instance,
        )?;

        let topic_root = &format!("devices/ugly-duckling/{}", config.instance);

        let uptime_us = unsafe { esp_idf_sys::esp_timer_get_time() };
        log::info!("Device started in {} ms", uptime_us as f64 / 1000.0);

        Ok(Self {
            config,
            _wifi: wifi,
            _sntp: sntp,
            topic_root: topic_root.clone(),
            mqtt: RefCell::new(mqtt),
        })
    }

    pub async fn publish_mqtt(&self, topic: &str, payload: Value) -> Result<MessageId> {
        self.publish_mqtt_internal(topic, &payload.to_string())
            .await
    }

    async fn publish_mqtt_internal(&self, topic: &str, payload: &str) -> Result<MessageId> {
        let result = self
            .mqtt
            .borrow_mut()
            .publish(
                &format!("{}/{}", self.topic_root, topic),
                esp_idf_svc::mqtt::client::QoS::AtLeastOnce,
                false,
                payload.to_string().as_bytes(),
            )
            .await?;
        Ok(result)
    }
}

fn load_device_config() -> Result<DeviceConfig> {
    let config_file = include_str!("../../data/device-config.json");
    let config: DeviceConfig = serde_json::from_str(config_file)?;
    log::info!("Loaded config: {:?}", config);
    Ok(config)
}
