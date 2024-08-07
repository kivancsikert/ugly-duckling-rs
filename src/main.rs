mod network;

use esp_idf_svc::hal::task;
use esp_idf_svc::sntp;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use network::{query_mdns, Service};
use std::future::pending;

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("  ______                   _    _       _");
    log::info!(" |  ____|                 | |  | |     | |");
    log::info!(" | |__ __ _ _ __ _ __ ___ | |__| |_   _| |__");
    log::info!(" |  __/ _` | '__| '_ ` _ \\|  __  | | | | '_ \\");
    log::info!(" | | | (_| | |  | | | | | | |  | | |_| | |_) |");
    log::info!(
        " |_|  \\__,_|_|  |_| |_| |_|_|  |_|\\__,_|_.__/ {}",
        env!("GIT_VERSION")
    );

    let sys_loop = EspSystemEventLoop::take()?;
    let timer_service = EspTaskTimerService::new()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let wifi = task::block_on(network::connect_wifi(&sys_loop, &timer_service, &nvs))?;
    let ip_info = wifi.sta_netif().get_ip_info()?;
    log::info!("WiFi DHCP info: {:?}", ip_info);

    let _sntp = sntp::EspSntp::new_with_callback(&Default::default(), |duration| {
        log::info!("SNTP time: {:?}", duration);
    })?;
    log::info!("Current time: {:?}", std::time::SystemTime::now());

    let mqtt = query_mdns("_mqtt", "_tcp")?.unwrap_or_else(|| Service {
        hostname: String::from("bumblebee"),
        port: 1883,
    });
    log::info!("MDNS query result: {:?}", mqtt);

    let (mut mqtt, _) = network::connect_mqtt(
        &format!("mqtt://{}.local:{}", mqtt.hostname, mqtt.port),
        "ugly-duckling-rs-test",
    )?;
    let payload = "Hello via mDNS!".as_bytes();
    task::block_on(mqtt.publish(
        "test",
        esp_idf_svc::mqtt::client::QoS::AtLeastOnce,
        false,
        payload,
    ))?;

    log::info!("Entering idle loop...");
    task::block_on(pending::<()>());
    Ok(())
}
