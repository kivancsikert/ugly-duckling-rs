mod network;

use anyhow::Result;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::channel::Receiver;
use embassy_sync::channel::Sender;
use embassy_sync::signal::Signal;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::mqtt::client::EspAsyncMqttClient;
use esp_idf_svc::sntp::{self, EspSntp};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::EspWifi;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use esp_idf_sys::{esp_pm_config_esp32_t, esp_pm_configure};
use network::{query_mdns, Service};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use static_cell::StaticCell;
use std::ffi::c_void;
use std::time::Duration;
use std::time::UNIX_EPOCH;

// TODO Configure these per device model
const MAX_FREQ_MHZ: i32 = 160;
const MIN_FREQ_MHZ: i32 = 40;

struct MqttMessage {
    topic: String,
    payload: Value,
}

const MQTT_QUEUE_SIZE: usize = 16;
type MqttPublishChannel = Channel<CriticalSectionRawMutex, MqttMessage, MQTT_QUEUE_SIZE>;
type MqttPublisher = Sender<'static, CriticalSectionRawMutex, MqttMessage, MQTT_QUEUE_SIZE>;
type MqttPublishReceiver = Receiver<'static, CriticalSectionRawMutex, MqttMessage, MQTT_QUEUE_SIZE>;

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
    mqtt_publisher: MqttPublisher,
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
        let (sntp, mqtt_publisher) =
            join(init_rtc(&mdns), init_mqtt(&mdns, &config.instance)).await;

        Ok(Self {
            config,
            _wifi: wifi,
            _sntp: sntp?,
            mqtt_publisher: mqtt_publisher?,
        })
    }

    pub async fn publish_mqtt(&self, topic: &str, payload: Value) -> Result<()> {
        self.mqtt_publisher
            .send(MqttMessage {
                topic: topic.to_string(),
                payload,
            })
            .await;
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

async fn init_mqtt(mdns: &EspMdns, instance: &str) -> Result<MqttPublisher> {
    let mqtt = query_mdns(mdns, "_mqtt", "_tcp")?.unwrap_or_else(|| Service {
        hostname: String::from("bumblebee.local"),
        port: 1883,
    });
    log::info!("MDNS query result: {:?}", mqtt);

    let (mqtt, _) =
        network::connect_mqtt(&format!("mqtt://{}:{}", mqtt.hostname, mqtt.port), instance)
            .expect("Couldn't connect to MQTT");
    static MQTT: StaticCell<EspAsyncMqttClient> = StaticCell::new();
    let mqtt = MQTT.init(mqtt);

    static MQTT_PUBLISH_QUEUE: StaticCell<MqttPublishChannel> = StaticCell::new();
    let publish_queue = MQTT_PUBLISH_QUEUE.init(MqttPublishChannel::new());
    let topic_root = format!("devices/ugly-duckling/{}", instance);

    Spawner::for_current_executor()
        .await
        .spawn(mqtt_publish_queue_task(
            mqtt,
            topic_root,
            publish_queue.receiver(),
        ))
        .expect("Couldn't start MQTT queue");

    Ok(publish_queue.sender())
}

#[embassy_executor::task]
async fn mqtt_publish_queue_task(
    mqtt: &'static mut EspAsyncMqttClient,
    topic_root: String,
    queue: MqttPublishReceiver,
) {
    loop {
        let message = queue.receive().await;
        mqtt.publish(
            &format!("{}/{}", &topic_root, &message.topic),
            esp_idf_svc::mqtt::client::QoS::AtLeastOnce,
            false,
            message.payload.to_string().as_bytes(),
        )
        .await
        .expect("Failed to publish message");
    }
}

fn load_device_config() -> Result<DeviceConfig> {
    let config_file = include_str!("../../data/device-config.json");
    let config: DeviceConfig = serde_json::from_str(config_file)?;
    log::info!("Loaded config: {:?}", config);
    Ok(config)
}
