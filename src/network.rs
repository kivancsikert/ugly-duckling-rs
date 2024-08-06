
use embedded_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};

use esp_idf_svc::wifi::{AsyncWifi, EspWifi};
use esp_idf_svc::wifi::{WpsConfig, WpsFactoryInfo, WpsStatus, WpsType};

const WPS_CONFIG: WpsConfig = WpsConfig {
    wps_type: WpsType::Pbc,
    factory_info: WpsFactoryInfo {
        manufacturer: "ESPRESSIF",
        model_number: "esp32",
        model_name: "ESPRESSIF IOT",
        device_name: "ESP DEVICE",
    },
};

pub async fn connect_wps(wifi: &mut AsyncWifi<EspWifi<'static>>) -> anyhow::Result<()> {
    wifi.start().await?;
    log::info!("Wifi started");

    match wifi.start_wps(&WPS_CONFIG).await? {
        WpsStatus::SuccessConnected => (),
        WpsStatus::SuccessMultipleAccessPoints(credentials) => {
            log::info!("received multiple credentials, connecting to first one:");
            for i in &credentials {
                log::info!(" - ssid: {}", i.ssid);
            }
            let wifi_configuration: Configuration = Configuration::Client(ClientConfiguration {
                ssid: credentials[0].ssid.clone(),
                bssid: None,
                auth_method: AuthMethod::WPA2Personal,
                password: credentials[1].passphrase.clone(),
                channel: None,
                ..Default::default()
            });
            wifi.set_configuration(&wifi_configuration)?;
        }
        WpsStatus::Failure => anyhow::bail!("WPS failure"),
        WpsStatus::Timeout => anyhow::bail!("WPS timeout"),
        WpsStatus::Pin(_) => anyhow::bail!("WPS pin"),
        WpsStatus::PbcOverlap => anyhow::bail!("WPS PBC overlap"),
    }

    match wifi.get_configuration()? {
        Configuration::Client(config) => {
            log::info!("Successfully connected to {} using WPS", config.ssid)
        }
        _ => anyhow::bail!("Not in station mode"),
    };

    wifi.connect().await?;
    log::info!("Wifi connected");

    wifi.wait_netif_up().await?;
    log::info!("Wifi netif up");

    Ok(())
}
