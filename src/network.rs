use embedded_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};

use esp_idf_svc::hal::prelude::Peripherals;
use esp_idf_svc::mqtt::client::{
    EspAsyncMqttClient, EspAsyncMqttConnection, MqttClientConfiguration,
};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AsyncWifi, EspWifi};
use esp_idf_svc::wifi::{WpsConfig, WpsFactoryInfo, WpsStatus, WpsType};
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};

const WPS_CONFIG: WpsConfig = WpsConfig {
    wps_type: WpsType::Pbc,
    factory_info: WpsFactoryInfo {
        manufacturer: "ESPRESSIF",
        model_number: "esp32",
        model_name: "ESPRESSIF IOT",
        device_name: "ESP DEVICE",
    },
};

pub async fn connect_wifi(
    sys_loop: &EspSystemEventLoop,
    timer_service: &EspTaskTimerService,
    nvs: &EspDefaultNvsPartition,
) -> anyhow::Result<EspWifi<'static>> {
    let peripherals = Peripherals::take()?;
    let mut esp_wifi = EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs.clone()))?;
    let mut wifi = AsyncWifi::wrap(&mut esp_wifi, sys_loop.clone(), timer_service.clone())?;

    wifi.start().await?;
    log::info!("Wifi started");

    match wifi.get_configuration()? {
        Configuration::Client(config) => {
            log::info!(
                "Using stored credentials to connect to SSID {}",
                config.ssid
            );
        }
        // TODO Is this the right way to handle mixed mode?
        Configuration::Mixed(client, _) => {
            log::info!(
                "Using stored credentials to connect to SSID {} (mixed)",
                client.ssid
            );
        }
        // TODO What should we do with AccessPoint?
        Configuration::None | Configuration::AccessPoint(_) => {
            match wifi.start_wps(&WPS_CONFIG).await? {
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
    };

    wifi.connect().await?;
    log::info!("Wifi connected");

    wifi.wait_netif_up().await?;
    log::info!("Wifi netif up");

    Ok(esp_wifi)
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
