use core::panic;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embedded_svc::ipv4::ClientConfiguration::DHCP;
use embedded_svc::ipv4::Configuration::Client;
use embedded_svc::ipv4::DHCPClientSettings;
use embedded_svc::wifi::Configuration;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::wifi::{
    AuthMethod, ClientConfiguration, EspWifi, WifiDriver, WifiEvent, WpsConfig, WpsFactoryInfo,
    WpsType,
};
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use esp_idf_sys::{esp_wifi_set_mode, wifi_mode_t_WIFI_MODE_STA};
use std::sync::Arc;

pub struct Wifi {
    _wifi: Arc<Mutex<CriticalSectionRawMutex, EspWifi<'static>>>,
}

impl Wifi {
    pub async fn create(
        device_name: &str,
        modem: Modem,
        sys_loop: &EspSystemEventLoop,
        nvs: &EspDefaultNvsPartition,
    ) -> anyhow::Result<Wifi> {
        let mut host_name: heapless::String<30> = heapless::String::new();
        host_name.push_str(device_name).expect("Hostname too long");
        let ipv4_client_cfg = DHCP(DHCPClientSettings {
            hostname: Some(host_name),
        });
        let netif_configuration = NetifConfiguration {
            ip_configuration: Client(ipv4_client_cfg),
            ..NetifConfiguration::wifi_default_client()
        };

        log::info!("Creating WiFi service");

        let wifi_driver = WifiDriver::new(modem, sys_loop.clone(), Some(nvs.clone()))
            .expect("Couldn't create WiFi driver");

        let wifi = EspWifi::wrap_all(
            wifi_driver,
            EspNetif::new_with_conf(&netif_configuration)?,
            EspNetif::new(NetifStack::Ap)?,
        )?;

        let wifi = Arc::new(Mutex::new(wifi));

        let wifi_connected = Arc::new(Signal::<CriticalSectionRawMutex, ()>::new());
        Spawner::for_current_executor()
            .await
            .spawn(wifi_event_task(
                device_name.to_string(),
                sys_loop.clone(),
                wifi.clone(),
                wifi_connected.clone(),
            ))
            .expect("Couldn't start WiFi event handler");
        wifi_connected.wait().await;

        Ok(Wifi { _wifi: wifi })
    }
}

#[embassy_executor::task]
async fn wifi_event_task(
    device_name: String,
    sys_loop: EspSystemEventLoop,
    wifi: Arc<Mutex<CriticalSectionRawMutex, EspWifi<'static>>>,
    wifi_ready: Arc<Signal<CriticalSectionRawMutex, ()>>,
) {
    log::info!("Starting WiFi event task");
    let mut events = sys_loop
        .subscribe_async::<WifiEvent>()
        .expect("Couldn't subscribe to system events");

    unsafe {
        esp_idf_sys::esp!(esp_wifi_set_mode(wifi_mode_t_WIFI_MODE_STA))
            .expect("Couldn't set WiFi mode");
    }

    {
        let mut wifi = wifi.lock().await;
        wifi.start().expect("Couldn't start STA");
    }
    log::info!("WiFi started");

    let mut connected = false;
    let mut wps_started = false;

    loop {
        let event = events.recv().await.expect("Couldn't receive WiFi event");
        log::info!("Received WiFi event: {:?}", event);
        match event {
            WifiEvent::StaStarted => {
                log::info!("STA Started");
                if wps_started {
                    continue;
                }
                let mut wifi = wifi.lock().await;
                let configuration = wifi
                    .get_configuration()
                    .expect("Couldn't get WiFi configuration");
                if has_stored_client_configuration(configuration) {
                    wifi.connect().expect("Couldn't start connection to WiFi");
                } else {
                    // Force STA mode
                    wifi.set_configuration(&Configuration::None).unwrap();
                    let wps_config = WpsConfig {
                        wps_type: WpsType::Pbc,
                        factory_info: WpsFactoryInfo {
                            manufacturer: "FarmHub",
                            model_name: "Ugly Duckling",
                            // TODO Set up the correct model number
                            model_number: "MK6",
                            device_name: &device_name,
                        },
                    };
                    wifi.start_wps(&wps_config).expect("Couldn't start WPS");
                    wps_started = true;
                    log::info!("WPS started");
                }
            }
            WifiEvent::StaWpsSuccess(credentials) => {
                wps_started = false;
                let mut wifi = wifi.lock().await;
                let status = wifi.stop_wps().expect("Couldn't stop WPS");
                log::info!("WPS finished with status: {:?}", status);
                if credentials.is_empty() {
                    log::info!("Received WPS credentials");
                } else {
                    log::info!("Received WPS credentials, connecting to first one:");
                    for i in credentials {
                        log::info!(" - ssid: {}", i.ssid().to_str().unwrap());
                    }

                    let mut ssid = heapless::String::new();
                    let mut password = heapless::String::new();
                    ssid.push_str(stringify!(credentials[0].ssid())).unwrap();
                    password
                        .push_str(stringify!(credentials[0].passphrase()))
                        .unwrap();
                    let wifi_configuration: Configuration =
                        Configuration::Client(ClientConfiguration {
                            ssid,
                            bssid: None,
                            auth_method: AuthMethod::WPA2Personal,
                            password,
                            channel: None,
                            ..Default::default()
                        });

                    wifi.set_configuration(&wifi_configuration).unwrap();
                }
                wifi.connect().expect("Couldn't start connection to WiFi");
            }
            WifiEvent::StaWpsFailed
            | WifiEvent::StaWpsTimeout
            | WifiEvent::StaWpsPin(_)
            | WifiEvent::StaWpsPbcOverlap => {
                // TODO Handle this better
                let wps_status = wifi.lock().await.stop_wps().unwrap();
                panic!("WPS failed: {:?}", wps_status);
            }
            WifiEvent::StaConnected => {
                log::info!("STA connected");
                connected = true;
            }
            WifiEvent::Ready => {
                wifi_ready.signal(());
                let ip_info = wifi
                    .lock()
                    .await
                    .sta_netif()
                    .get_ip_info()
                    .expect("Couldn't get IP info");
                log::info!("WiFi DHCP info: {:?}", ip_info);
            }
            WifiEvent::StaDisconnected => {
                if wps_started {
                    // Ignore disconnection event while WPS is running
                    continue;
                }
                if !connected {
                    // Ignore disconnection event if we're not connected
                    continue;
                }
                log::info!("STA disconnected, reconnecting");
                connected = false;
                if let Err(e) = wifi.lock().await.connect() {
                    log::error!("Couldn't reconnect to WiFi: {:?}", e);
                } else {
                    log::info!("STA reconnecting...");
                }
            }
            _ => {}
        }
    }
}

fn has_stored_client_configuration(wifi_config: Configuration) -> bool {
    match wifi_config {
        Configuration::Client(client) => has_stored_ssid(&client.ssid, ""),
        Configuration::Mixed(client, _) => has_stored_ssid(&client.ssid, " (mixed)"),
        Configuration::None | Configuration::AccessPoint(_) => {
            log::info!("No stored client credentials");
            false
        }
    }
}

fn has_stored_ssid(ssid: &str, suffix: &str) -> bool {
    if ssid.is_empty() {
        log::info!("No stored client credentials");
        false
    } else {
        log::info!("Using stored client credentials to connect to SSID {ssid}{suffix}");
        true
    }
}
