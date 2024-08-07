mod network;

use embassy_executor::{Executor, Spawner};
use embedded_hal_async::delay::DelayNs;
use esp_idf_hal::gpio::{AnyOutputPin, PinDriver};
use esp_idf_hal::prelude::Peripherals;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::sntp;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use network::{query_mdns, Service};
use static_cell::StaticCell;
use std::future::pending;

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

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

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner| {
        spawner.spawn(run(spawner)).unwrap();
    });
}

#[embassy_executor::task]
async fn run(spawner: Spawner) {
    match run_with_errors(spawner).await {
        Ok(_) => log::info!("Program exited cleanly"),
        Err(error) => log::error!("Program exited with error: {:?}", error),
    }
}

async fn run_with_errors(spawner: Spawner) -> anyhow::Result<()> {
    let peripherals = Peripherals::take()?;

    spawner
        .spawn(blink(peripherals.pins.gpio4.into()))
        .map_err(|error| anyhow::anyhow!("Failed to spawn blink task: {:?}", error))?;

    let sys_loop = EspSystemEventLoop::take()?;
    let timer_service = EspTaskTimerService::new()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let wifi = network::connect_wifi(peripherals.modem, &sys_loop, &timer_service, &nvs).await?;
    let ip_info = wifi.sta_netif().get_ip_info()?;
    log::info!("WiFi DHCP info: {:?}", ip_info);

    // TODO Use some async mDNS instead to avoid blocking the executor
    let mdns = EspMdns::take()?;

    let ntp = query_mdns(&mdns, "_ntp", "_udp")?.unwrap_or_else(|| Service {
        hostname: String::from("pool.ntp.org"),
        port: 123,
    });
    log::info!(
        "Time before SNTP sync: {:?}, synchronizing with {:?}",
        std::time::SystemTime::now(),
        ntp
    );

    let _sntp = sntp::EspSntp::new_with_callback(
        &sntp::SntpConf {
            servers: [ntp.hostname.as_str()],
            ..Default::default()
        },
        |duration| {
            log::info!("Time synced via SNTP: {:?}", duration);
        },
    )?;

    let mqtt = query_mdns(&mdns, "_mqtt", "_tcp")?.unwrap_or_else(|| Service {
        hostname: String::from("bumblebee.local"),
        port: 1883,
    });
    log::info!("MDNS query result: {:?}", mqtt);

    let (mut mqtt, _) = network::connect_mqtt(
        &format!("mqtt://{}:{}", mqtt.hostname, mqtt.port),
        "ugly-duckling-rs-test",
    )?;
    let payload = "Hello via mDNS!".as_bytes();
    mqtt.publish(
        "test",
        esp_idf_svc::mqtt::client::QoS::AtLeastOnce,
        false,
        payload,
    )
    .await?;

    let uptime_us = unsafe { esp_idf_sys::esp_timer_get_time() };
    log::info!("Device started in {} ms", uptime_us as f64 / 1000.0);

    log::info!("Entering idle loop...");
    pending::<()>().await;
    Ok(())
}

#[embassy_executor::task]
async fn blink(pin: AnyOutputPin) {
    log::info!("Blink task started");

    let timer_service = EspTaskTimerService::new().unwrap();
    let mut timer = timer_service.timer_async().unwrap();
    let mut status = PinDriver::output(pin).unwrap();
    loop {
        status.set_high().unwrap();
        timer.delay_ms(1000).await;

        status.set_low().unwrap();
        timer.delay_ms(1000).await;
    }
}
