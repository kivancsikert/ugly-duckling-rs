mod network;

use esp_idf_svc::hal::task;
use esp_idf_svc::sntp;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    mdns::{EspMdns, Interface, Protocol, QueryResult},
    nvs::EspDefaultNvsPartition,
};
use std::future::pending;
use std::time::Duration;

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

    let mdns = EspMdns::take()?;
    let mut results = [QueryResult {
        instance_name: None,
        hostname: None,
        port: 0,
        txt: Vec::new(),
        addr: Vec::new(),
        interface: Interface::STA,
        ip_protocol: Protocol::V4,
    }];

    let _sntp = sntp::EspSntp::new_with_callback(&Default::default(), |duration| {
        log::info!("SNTP time: {:?}", duration);
    })?;
    log::info!("Current time: {:?}", std::time::SystemTime::now());

    mdns.query_ptr(
        "_mqtt",
        "_tcp",
        Duration::from_secs(5),
        1,
        &mut results,
    )?;
    log::info!("MDNS query result: {:?}", results);
    let result = results[0].clone();
    let mqtt_host = result.hostname.unwrap();
    let mqtt_port = result.port;

    let (mut mqtt, _) = network::connect_mqtt(&format!("mqtt://{}.local:{}", mqtt_host, mqtt_port), "ugly-duckling-rs-test")?;
    let payload = "Hello mDNS!".as_bytes();
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
