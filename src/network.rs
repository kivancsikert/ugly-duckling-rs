use std::time::Duration;

use embedded_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};

use esp_idf_hal::modem::Modem;
use esp_idf_svc::mdns::{EspMdns, Interface, Protocol, QueryResult};
use esp_idf_svc::mqtt::client::{
    EspAsyncMqttClient, EspAsyncMqttConnection, MqttClientConfiguration,
};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AsyncWifi, EspWifi};
use esp_idf_svc::wifi::{WpsConfig, WpsFactoryInfo, WpsStatus, WpsType};
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};

pub async fn connect_wifi(
    modem: Modem,
    sys_loop: &EspSystemEventLoop,
    timer_service: &EspTaskTimerService,
    nvs: &EspDefaultNvsPartition,
) -> anyhow::Result<EspWifi<'static>> {
    let mut esp_wifi = EspWifi::new(modem, sys_loop.clone(), Some(nvs.clone()))?;
    let mut wifi = AsyncWifi::wrap(&mut esp_wifi, sys_loop.clone(), timer_service.clone())?;

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
                // TODO Set up the correct device name
                device_name: "test-mk6-rs-1",
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

    wifi.connect().await?;
    log::info!("Wifi connected");

    wifi.wait_netif_up().await?;
    log::info!("Wifi netif up");

    Ok(esp_wifi)
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

#[derive(Debug, Clone)]
pub struct Service {
    pub hostname: String,
    pub port: u16,
}

pub fn query_mdns(mdns: &EspMdns, service: &str, proto: &str) -> anyhow::Result<Option<Service>> {
    let mut results = [QueryResult {
        instance_name: None,
        hostname: None,
        port: 0,
        txt: Vec::new(),
        addr: Vec::new(),
        interface: Interface::STA,
        ip_protocol: Protocol::V4,
    }];
    mdns.query_ptr(service, proto, Duration::from_secs(5), 1, &mut results)?;
    log::info!("MDNS query result: {:?}", results);
    let result = results[0].clone();
    Ok(result.hostname.map(|hostname| Service {
        hostname: format!("{}.local", hostname),
        port: result.port,
    }))
}

pub fn connect_mqtt(
    url: &str,
    client_id: &str,
) -> anyhow::Result<(EspAsyncMqttClient, EspAsyncMqttConnection)> {
    let (mqtt_client, mqtt_conn) = EspAsyncMqttClient::new(
        url,
        &MqttClientConfiguration {
            client_id: Some(client_id),
            ..Default::default()
        },
    )?;

    Ok((mqtt_client, mqtt_conn))
}
