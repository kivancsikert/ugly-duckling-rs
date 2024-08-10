use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_svc::ipv4::ClientConfiguration::DHCP;
use embedded_svc::ipv4::Configuration::Client;
use embedded_svc::ipv4::DHCPClientSettings;
use embedded_svc::wifi::Configuration;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{
    AsyncWifi, AuthMethod, ClientConfiguration, EspWifi, WifiDriver, WifiEvent, WpsConfig,
    WpsFactoryInfo, WpsStatus, WpsType,
};
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use std::sync::Arc;

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
