mod network;

use anyhow::Result;
use embassy_executor::Executor;
use embassy_futures::join::join_array;
use embassy_futures::select::select;
use embassy_futures::select::Either::First;
use embedded_hal_async::delay::DelayNs;
use esp_idf_hal::gpio::{AnyIOPin, AnyOutputPin, IOPin, PinDriver};
use esp_idf_hal::modem::Modem;
use esp_idf_hal::prelude::Peripherals;
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::sntp;
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition};
use network::{query_mdns, Service};
use static_cell::StaticCell;
use std::future::{pending, Future};
use std::pin::Pin;

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

fn main() -> Result<()> {
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
        spawner.spawn(run_tasks()).unwrap();
    });
}

macro_rules! task {
    ($task:expr) => {
        Box::pin(task($task)) as Pin<Box<dyn Future<Output = ()>>>
    };
}

#[embassy_executor::task]
async fn run_tasks() {
    let peripherals = Peripherals::take().expect("Failed to take peripherals");

    let tasks: [Pin<Box<dyn Future<Output = ()>>>; 3] = [
        task!(blink(peripherals.pins.gpio4.into())),
        task!(reset_watcher(peripherals.pins.gpio0.downgrade())),
        task!(start_device(peripherals.modem)),
    ];
    join_array(tasks).await;
}

async fn task<Fut: Future<Output = Result<()>>>(future: Fut) {
    let result = future.await;
    match result {
        Ok(_) => log::info!("Task completed successfully"),
        Err(e) => log::error!("Task failed with {:?}", e),
    };
}

async fn start_device(modem: Modem) -> Result<()> {
    let sys_loop = EspSystemEventLoop::take()?;
    let timer_service = EspTaskTimerService::new()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let wifi = network::connect_wifi(modem, &sys_loop, &timer_service, &nvs).await?;
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

async fn reset_watcher(pin: AnyIOPin) -> Result<()> {
    let mut button = PinDriver::input(pin)?;
    button.set_pull(esp_idf_hal::gpio::Pull::Up)?;
    log::info!("Factory reset watcher started");
    loop {
        button.wait_for_low().await?;
        log::info!("Button pressed, waiting for release");

        let timer_service = EspTaskTimerService::new()?;
        let mut timer = timer_service.timer_async()?;

        if let First(_) = select(timer.delay_ms(5000), button.wait_for_high()).await {
            log::info!("Button pressed for more than 5 seconds, resetting WiFi");
            unsafe { esp_idf_sys::esp_wifi_restore() };
            unsafe { esp_idf_sys::esp_restart() };
        }
    }
}

async fn blink(pin: AnyOutputPin) -> Result<()> {
    log::info!("Blink task started");

    let timer_service = EspTaskTimerService::new()?;
    let mut timer = timer_service.timer_async()?;
    let mut status = PinDriver::output(pin)?;
    loop {
        status.set_high()?;
        timer.delay_ms(1000).await;

        status.set_low()?;
        timer.delay_ms(1000).await;
    }
}
