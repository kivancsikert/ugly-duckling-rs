mod network;

use anyhow::Result;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::mqtt::client::EspAsyncMqttClient;
use esp_idf_svc::mqtt::client::EspAsyncMqttConnection;
use esp_idf_svc::mqtt::client::EventPayload;
use esp_idf_svc::sntp::{self, EspSntp};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::EspWifi;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use esp_idf_sys::{esp_pm_config_esp32_t, esp_pm_configure};
use network::{query_mdns, Service};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::Duration;
use std::time::UNIX_EPOCH;

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
    _wifi: EspWifi<'static>,
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
            network::connect_wifi(&config.instance, modem, &sys_loop, &timer_service, &nvs).await?;
        let ip_info = wifi.sta_netif().get_ip_info()?;
        log::info!("WiFi DHCP info: {:?}", ip_info);

        // TODO Use some async mDNS instead to avoid blocking the executor
        let mdns = EspMdns::take()?;
        let (sntp, mqtts) = join(init_rtc(&mdns), init_mqtt(&mdns, &config.instance)).await;
        let (mqtt, conn, topic_root) = mqtts?;

        // TODO Figure out how to avoid this warning
        #[allow(clippy::arc_with_non_send_sync)]
        let mqtt = Arc::new(Mutex::new(mqtt));
        // TODO Figure out how to avoid this warning
        #[allow(clippy::arc_with_non_send_sync)]
        let conn = Arc::new(Mutex::new(conn));

        Spawner::for_current_executor()
            .await
            .spawn(handle_mqtt_events(conn.clone()))
            .expect("Couldn't spawn MQTT handler");

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
                esp_idf_svc::mqtt::client::QoS::AtMostOnce,
                false,
                payload.to_string().as_bytes(),
            )
            .await?;
        Ok(())
    }
}

async fn init_rtc(mdns: &EspMdns) -> Result<EspSntp<'static>> {
    let ntp = query_mdns(mdns, "_ntp", "_udp")?.unwrap_or_else(|| Service {
        hostname: String::from("pool.ntp.org"),
        port: 123,
    });

    log::info!(
        "Time before SNTP sync: {:?}, synchronizing with {:?}",
        std::time::SystemTime::now(),
        ntp
    );

    static SNTP_UPDATED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
    let sntp = sntp::EspSntp::new_with_callback(
        &sntp::SntpConf {
            servers: [ntp.hostname.as_str()],
            ..Default::default()
        },
        |synced_time| {
            SNTP_UPDATED.signal(());
            log::info!("Time synced via SNTP: {:?}", synced_time);
        },
    )?;
    // When the RTC is not initialized, the MCU boots with a time of 0;
    // if we are much "later" then it means the RTC has been initialized
    // at some point in the past
    if std::time::SystemTime::now()
        < UNIX_EPOCH + Duration::from_secs((2022 - 1970) * 365 * 24 * 60 * 60)
    {
        log::info!("RTC is not initialized, waiting for SNTP sync");
        SNTP_UPDATED.wait().await;
    } else {
        log::info!("RTC seems to be initialized already, not waiting for SNTP sync");
    }
    Ok(sntp)
}

async fn init_mqtt(
    mdns: &EspMdns,
    instance: &str,
) -> Result<(EspAsyncMqttClient, EspAsyncMqttConnection, String)> {
    let mqtt = query_mdns(mdns, "_mqtt", "_tcp")?.unwrap_or_else(|| Service {
        hostname: String::from("bumblebee.local"),
        port: 1883,
    });
    log::info!("MDNS query result: {:?}", mqtt);

    let (mqtt, conn) =
        network::connect_mqtt(&format!("mqtt://{}:{}", mqtt.hostname, mqtt.port), instance)
            .expect("Couldn't connect to MQTT");

    let topic_root = format!("devices/ugly-duckling/{}", instance);

    Ok((mqtt, conn, topic_root))
}

#[embassy_executor::task]
async fn handle_mqtt_events(conn: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>) {
    let mut conn = conn.lock().await;
    loop {
        let event = conn.next().await.expect("Cannot receive message");
        match event.payload() {
            EventPayload::Received {
                id,
                topic,
                data,
                details,
            } => {
                log::info!(
                    "Received message with ID {} on topic {:?}: {:?}; details: {:?}",
                    id,
                    topic,
                    std::str::from_utf8(data),
                    details
                );
            }
            EventPayload::Disconnected => {
                log::info!("Disconnected from MQTT broker");
                // TODO Reconnect
            }
            _ => {
                log::info!("Received event: {:?}", event.payload());
            }
        }
    }
}

fn load_device_config() -> Result<DeviceConfig> {
    let config_file = include_str!("../../data/device-config.json");
    let config: DeviceConfig = serde_json::from_str(config_file)?;
    log::info!("Loaded config: {:?}", config);
    Ok(config)
}
