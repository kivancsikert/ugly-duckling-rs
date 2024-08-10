use std::time::Duration;

use crate::kernel::mdns;
use anyhow::Result;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embedded_svc::ipv4::ClientConfiguration::DHCP;
use embedded_svc::ipv4::Configuration::Client;
use embedded_svc::ipv4::DHCPClientSettings;
use embedded_svc::wifi::Configuration;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::mqtt::client::EventPayload;
use esp_idf_svc::mqtt::client::{
    EspAsyncMqttClient, EspAsyncMqttConnection, MqttClientConfiguration,
};
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::sntp::{self, EspSntp};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{
    AsyncWifi, AuthMethod, ClientConfiguration, EspWifi, WifiDriver, WifiEvent, WpsConfig,
    WpsFactoryInfo, WpsStatus, WpsType,
};
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

pub async fn init_wifi(
    device_name: &str,
    modem: Modem,
    sys_loop: &EspSystemEventLoop,
    timer_service: &EspTaskTimerService,
    nvs: &EspDefaultNvsPartition,
) -> anyhow::Result<Arc<Mutex<CriticalSectionRawMutex, AsyncWifi<EspWifi<'static>>>>> {
    let mut host_name: heapless::String<30> = heapless::String::new();
    host_name.push_str(device_name).expect("Hostname too long");
    let ipv4_client_cfg = DHCP(DHCPClientSettings {
        hostname: Some(host_name),
    });
    let new_c = NetifConfiguration {
        ip_configuration: Client(ipv4_client_cfg),
        ..NetifConfiguration::wifi_default_client()
    };

    let esp_wifi = EspWifi::wrap_all(
        WifiDriver::new(modem, sys_loop.clone(), Some(nvs.clone()))?,
        EspNetif::new_with_conf(&new_c)?,
        EspNetif::new(NetifStack::Ap)?,
    )?;
    let wifi = AsyncWifi::wrap(esp_wifi, sys_loop.clone(), timer_service.clone())?;

    let wifi_arc = Arc::new(Mutex::new(wifi));
    Spawner::for_current_executor()
        .await
        .spawn(wifi_event_task(sys_loop.clone(), wifi_arc.clone()))
        .unwrap();

    let mut wifi = wifi_arc.lock().await;

    wifi.start().await?;
    log::info!("Wifi started");

    if !has_stored_client_configuration(wifi.get_configuration()?) {
        let wps_config = WpsConfig {
            wps_type: WpsType::Pbc,
            factory_info: WpsFactoryInfo {
                manufacturer: "FarmHub",
                model_name: "Ugly Duckling",
                // TODO Set up the correct model number
                model_number: "MK6",
                device_name,
            },
        };

        log::info!("Starting WPS");
        match wifi.start_wps(&wps_config).await? {
            WpsStatus::SuccessConnected => (),
            WpsStatus::SuccessMultipleAccessPoints(credentials) => {
                log::info!("Received multiple credentials, connecting to first one:");
                for i in &credentials {
                    log::info!(" - ssid: {}", i.ssid);
                }
                let ssid = &credentials[0].ssid;
                let wifi_configuration: Configuration =
                    Configuration::Client(ClientConfiguration {
                        ssid: ssid.clone(),
                        bssid: None,
                        auth_method: AuthMethod::WPA2Personal,
                        password: credentials[1].passphrase.clone(),
                        channel: None,
                        ..Default::default()
                    });
                wifi.set_configuration(&wifi_configuration)?;
                log::info!("Successfully connected to {} via WPS", ssid);
            }
            WpsStatus::Failure => anyhow::bail!("WPS failure"),
            WpsStatus::Timeout => anyhow::bail!("WPS timeout"),
            WpsStatus::Pin(_) => anyhow::bail!("WPS pin"),
            WpsStatus::PbcOverlap => anyhow::bail!("WPS PBC overlap"),
        }
    }

    log::info!("Connecting to WiFi");
    wifi.connect().await?;
    log::info!("Wifi connected");

    wifi.wait_netif_up().await?;
    log::info!("Wifi netif up");

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    log::info!("WiFi DHCP info: {:?}", ip_info);

    Ok(wifi_arc.clone())
}

#[embassy_executor::task]
async fn wifi_event_task(
    sys_loop: EspSystemEventLoop,
    wifi: Arc<Mutex<CriticalSectionRawMutex, AsyncWifi<EspWifi<'static>>>>,
) {
    let mut events = sys_loop
        .subscribe_async::<WifiEvent>()
        .expect("Couldn't subscribe to system events");

    loop {
        let event = events.recv().await.expect("Couldn't receive event");
        log::info!("Received wifi event: {:?}", event);
        match event {
            WifiEvent::StaConnected => {
                log::info!("STA connected");
            }
            WifiEvent::StaDisconnected => {
                log::info!("STA disconnected, reconnecting");
                wifi.lock()
                    .await
                    .connect()
                    .await
                    .expect("Couldn't start reconnection to WiFi");
                log::info!("STA reconnecting...");
            }
            _ => {}
        }
    }
}

fn has_stored_client_configuration(wifi_config: Configuration) -> bool {
    match wifi_config {
        Configuration::Client(config) => {
            log::info!(
                "Using stored client credentials to connect to SSID {}",
                config.ssid
            );
            true
        }
        Configuration::Mixed(client, _) => {
            if client.ssid.is_empty() {
                log::info!("No stored client credentials (mixed)");
                false
            } else {
                log::info!(
                    "Using stored client credentials to connect to SSID {} (mixed)",
                    client.ssid
                );
                true
            }
        }
        Configuration::None | Configuration::AccessPoint(_) => {
            log::info!("No stored client credentials");
            false
        }
    }
}

pub async fn init_rtc(mdns: &EspMdns) -> Result<EspSntp<'static>> {
    let ntp = mdns::query_mdns(mdns, "_ntp", "_udp")?.unwrap_or_else(|| mdns::Service {
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

pub async fn init_mqtt(
    mdns: &EspMdns,
    instance: &str,
) -> Result<(
    EspAsyncMqttClient,
    Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>,
    String,
)> {
    let mqtt = mdns::query_mdns(mdns, "_mqtt", "_tcp")?.unwrap_or_else(|| mdns::Service {
        hostname: String::from("bumblebee.local"),
        port: 1883,
    });
    log::info!("MDNS query result: {:?}", mqtt);

    let url: &str = &format!("mqtt://{}:{}", mqtt.hostname, mqtt.port);
    let (mqtt, conn) = EspAsyncMqttClient::new(
        url,
        &MqttClientConfiguration {
            client_id: Some(instance),
            ..Default::default()
        },
    )
    .expect("Couldn't connect to MQTT");

    let topic_root = format!("devices/ugly-duckling/{}", instance);

    // TODO Figure out how to avoid this warning
    #[allow(clippy::arc_with_non_send_sync)]
    let conn = Arc::new(Mutex::new(conn));

    // TODO Need something more robust than this, but for the time being it will do
    let connected = Arc::new(Signal::<CriticalSectionRawMutex, ()>::new());
    Spawner::for_current_executor()
        .await
        .spawn(handle_mqtt_events(conn.clone(), connected.clone()))
        .expect("Couldn't spawn MQTT handler");
    connected.wait().await;

    Ok((mqtt, conn, topic_root))
}

#[embassy_executor::task]
async fn handle_mqtt_events(
    conn: Arc<Mutex<CriticalSectionRawMutex, EspAsyncMqttConnection>>,
    connected: Arc<Signal<CriticalSectionRawMutex, ()>>,
) {
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
            EventPayload::Connected(session_present) => {
                log::info!(
                    "Connected to MQTT broker (session present: {})",
                    session_present
                );
                connected.signal(());
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
