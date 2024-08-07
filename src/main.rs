mod network;

use std::future::pending;

use esp_idf_svc::hal::task;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("Hello, world!");

    let sys_loop = EspSystemEventLoop::take()?;
    let timer_service = EspTaskTimerService::new()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let wifi = task::block_on(network::connect_wifi(&sys_loop, &timer_service, &nvs))?;

    let ip_info = wifi.sta_netif().get_ip_info()?;
    log::info!("Wifi DHCP info: {:?}", ip_info);

    log::info!("Entering idle loop...");

    task::block_on(pending::<()>());
    Ok(())
}
